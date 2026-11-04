# CMX918 register investigation

These notes record differences between published CMX918 documentation and
the evaluation software, checked on 2026-10-07, plus a later user oscilloscope
observation. They are research findings, not a replacement register specification.

**A field present in software is not proof that it works on every silicon
revision.** Names, addresses and masks below are taken from software definitions;
electrical behavior and polarity are marked separately where unknown. The
evaluation-software comparison did not involve undocumented register writes.

## Sources and method

| Reference | Source | Use |
|---|---|---|
| DS | Local `CMX918_DS_V2.0.pdf` | Current pin assignments and interface description |
| UM | User-supplied `CMX918_UM_V2.0.pdf`, UM918/2.0 | Detailed register definitions; page numbers below are printed page numbers |
| ES | `ES9180v1_0.zip` evaluation software, installer `ES9180InstallerV1p0.exe` | Register and field tables bundled in `ES9180.exe` |
| HW | Read-only access through Astra918's register helper scripts | Observations on one connected receiver, not reset-value or electrical characterization |
| Scope | User's oscilloscope measurement during an earlier pilot-output scan | Pilot output observed on pin 8 in scan state 5 |

The register descriptions are primarily in **UM**, rather than DS. Calling a
field undocumented here means it is missing from, or explicitly marked unused
in, that register manual. Fields that are merely underexplained are separated
below.

The ES installer archive was unpacked. Its `registers` and `fields` table modules
were evaluated with inert constructor replacements, restricted builtins and an
import hook allowing only those replacements. The evaluation GUI and hardware-interface
code were not run, and instructions were not disassembled. The comparison covered
140 named register entries and 226 named field entries, checking apparent name
differences against the manual's bit diagrams and prose.

Source fingerprints (SHA-256):

```text
CMX918_DS_V2.0.pdf
a7d74d0d25026df0d6f2e1c884b44bff15f02fb443d389db762e7d21133ace06
CMX918_UM_V2.0.pdf
4117467d95306d69302122f09e85fa23735f03dd3d6c86ae1fd38fcae92cf0ff
ES9180InstallerV1p0.exe
d9c098c1dc34a274dd538369826f204ab185f765a9521258c88be749bcf54de0
```

## Pilot signal on pin 8 — observed scan state 5

**Evidence: user oscilloscope observation on an Astra918 receiver.** While
running an earlier pilot-output scan, the user
observed a signal near **9.9 MHz on CMX918 pin 8** at **state 5/10**. This is
the divided-pilot test state; the script does not enable the documented
19.2 MHz reference-clock output on pin 8.

| Register | State 5 value | Relevant setting |
|---|---:|---|
| `$07` / `CLK_CTL` | bit 1 = `0` | `PLL_CFB_EN` off; the 19.2 MHz clock buffer is disabled. Other bits retain their previous values. |
| `$98` / `PLL_PILOT_CTL` | `$01` | Bit 0 `PLL_PILOT_EN` on; bits 1–2 RF pilot injection off. |
| `$99`, `$9A` / `PLL_PILOT_N` | `$00`, `$0F` | Pilot N divider = 15. |
| `$9B`, `$9C` / `PLL_PILOT_P` | `$00`, `$1D` | Pilot P divider = 29. |
| `$9E` / `PILOT_TEST_SW` | `$04` | **Bit 2 `PILOT_OUT_SW_EN` on**; bit 1 `PILOT_VCO_OUT_SW_EN` and bit 0 `PILOT_VCTRL_SW_EN` off. |

With the 38.4 MHz reference, the documented divider formula gives
`38.4 MHz × 15 / (2 × 29) = 9.931034 MHz`, consistent with the approximate
oscilloscope reading. The script checked `$06` bit 6 for pilot PLL lock before
entering the scan. In state 5, `$9E` bit 2 is the selected test-output switch;
this is the bit combination associated with the observed pin-8 signal. The
pilot PLL enable at `$98` bit 0 and the divider registers are also necessary
for that frequency. This observation establishes the behavior on the tested
receiver; it does not establish output level, drive capability, or support on
every silicon revision. The current datasheet describes pin 8 as `CLK_OUT /
EXT_LO_IN` and does not document a pilot-output function there.

## L-divider test-enable search: rejected `$9E` upper-bit mapping

The user's transcription of a test-enable block names `VCO_OUT_SW_EN`,
`L_DIV_SW_EN`, `PILOT_OUT_SW_EN`, `PILOT_VCO_OUT_SW_EN`, `PILOT_VCTR_SW_EN`,
and `ADC_CLOCK_SW_EN`. Current DS Figure 9 instead labels `PLL_MON_EN` and
`PLL_VCTRL_SW_EN` in that area and does not explicitly name `L_DIV_SW_EN`;
the diagrams may differ. UM page 63 maps only three pilot switches to `$9E`
bits 2, 1 and 0. The first hypothesis placed the remaining switches at `$9E`
bits 3–5. The user's repeated L-divider scan rejected
that hypothesis on the tested receiver:

| `$9E` request | Readback, both cycles | Signal on pin 8 | Interpretation |
|---:|---:|---|---|
| `$04`, documented `PILOT_OUT_SW_EN` | `$04` | About 9.9 MHz | Confirmed reference state |
| `$00`, switches off | `$00` | None | Baseline |
| `$08`, guessed `L_DIV_SW_EN` | `$00` | None | Bit 3 did not latch |
| `$10`, guessed `VCO_OUT_SW_EN` | `$00` | None | Bit 4 did not latch |
| `$20`, guessed `ADC_CLOCK_SW_EN` | `$00` | None | Bit 5 did not latch |

The script reported **main PLL locked at every state**. Separately, a receiver
status query after the scan returned `$06=$39`, also setting the main PLL lock
bit (`$06` bit 3). This rules out an unlocked main PLL as the immediate cause
of the blank candidate states. UM explicitly marks `$9E` bits 3–7 *reserved,
clear to zero*; bits 3–5 did not latch in the readable register state and did
not expose a signal on pin 8. A write-only internal side effect cannot be ruled
out, but these are poor output-switch candidates. Bits 6–7 have not been
tested, and there is no evidence yet that they map to test outputs.

The location of `L_DIV_SW_EN` remains **unknown**. It may be controlled by
another register or not exposed by this silicon revision. The published
register map and the examined ES9180 field definitions provide no named
L-divider test-output control. `$2A` bit 1 selects the L-divider's automatic
versus manual ratio source; `$31`–`$32` hold its ratio. None of those documented
fields enables a test output. Avoid assigning an address or bit to
`L_DIV_SW_EN` without further register documentation or a controlled pin
measurement.

### Reserved PLL-bit readback scan on the connected receiver

An earlier reserved-bit scan tested another 15 bits,
one at a time, preserving the documented fields in each register. UM marks all
of these bits *reserved, clear to zero*. The receiver was idle (no I/Q stream);
no pilot/reference generator was configured for this test. The script wrote
each bit, read back all four registers and `$06` status, then restored the
original value before the next candidate.

| Register | Original | Bits requested individually | Readback with each bit requested |
|---|---:|---|---:|
| `$92` / `PLL_VCO_BC` | `$03` | 4, 5, 6 | `$03` |
| `$91` / `PLL_LF_CTL_2` | `$1F` | 5, 6, 7 | `$1F` |
| `$90` / `PLL_LF_CTL_1` | `$03` | 2, 3, 4, 5, 6, 7 | `$03` |
| `$8E` / `PLL_CP_CTL` | `$23` | 2, 3, 7 | `$23` |

None of the requested bits latched in the readable register state. `$06` was
`$39` (main PLL locked) at baseline and in 14 candidate states. During the
`$90` bit 6 state, one status read was `$33` (main PLL lock bit clear), but a
subsequent read after restoring the registers was `$39`. This single unlock
sample does not establish whether bit 6 caused a transient or whether it was
unrelated. The original register values were restored and verified. No
oscilloscope observation was made for these 15 states; a write-only side effect
cannot be excluded from readback alone. The scan does **not** identify an
`L_DIV_SW_EN` control.

### Unused-register search for main-PLL test switches

The synthesizer block diagram shows `VCO_OUT_SW_EN`, `L_DIV_SW_EN` and
`ADC_CLOCK_SW_EN` alongside the three pilot switches. The ES9180
field table contains only the three `PILOT_*_SW_EN` names at `$9E` bits 2–0;
it does not assign addresses or masks to the other diagram labels. The diagram
shows internal signals, not a register map or a guaranteed path to pin 8.

UM calls `$83–$8D` and `$A0` unused. A **read-only** snapshot on the connected
receiver, with the normal firmware running, returned:

| Register(s) | Readback | Location in the UM map |
|---|---|---|
| `$83`, `$84` | `$00`, `$00` | Start of the service-settings gap, before PLL charge-pump control `$8E` |
| `$85–$89` | `$C0`, `$40`, `$40`, `$40`, `$40` | Same gap |
| `$8A–$8D` | `$1F`, `$03`, `$F9`, `$45` | Same gap |
| `$A0` | `$00` | Between RF AGC controls `$9F` and `$A1` |

The `$83–$8D` gap is a more plausible *search area* than `$A0` because of its
position immediately before the documented PLL analogue-control registers.
Its nonzero bytes show only that some readable state exists there; they may be
trims, status, or something else. Zero readback at `$83`, `$84`, or `$A0` does
not establish that any bit is writable. No writes to these undocumented
addresses or pin-8 measurements were made in this search. In particular, it
would be unsound to clear the nonzero bytes or call any bit `L_DIV_SW_EN`
without more evidence.

## $DA — I2S_PAD_CTL

**Evidence: exact software definition; hardware readback only.** UM page 81
labels `$DA` as unused. ES instead defines an 8-bit read/write register named
`I2S_PAD_CTL`, with a software reset value of `$00`.

ES puts it in its page-1 table at address `$5A`, with display/readback address
`$DA`. For Astra918's direct I2C register interface and helper scripts, use
**`0xDA`**. Do not substitute `0x5A`: that is the separate `DPB_I2S_CTL_0`
register in the direct I2C address space.

| Bit | Mask | ES field name | Interpretation from the name |
|---:|---:|---|---|
| 7–6 | `0xC0` | No named fields | Meaning unknown |
| 5 | `0x20` | `I2S_SCLK_OEN` | Clock output-driver control |
| 4 | `0x10` | `I2S_SCLK_IE` | Clock input-buffer enable |
| 3 | `0x08` | `I2S_FRAME_OEN` | Frame/CS output-driver control |
| 2 | `0x04` | `I2S_FRAME_IE` | Frame/CS input-buffer enable |
| 1 | `0x02` | `I2S_DATA_OEN` | Data output-driver control |
| 0 | `0x01` | `I2S_DATA_IE` | Data input-buffer enable |

The software's constructor records are equivalent to:

```text
Register(addr=0x5A, size=8, rw="rw", reset_val=0x00,
         readback_addr=0xDA, display_addr=0xDA)  # page-1 table
RegField(registers=["I2S_PAD_CTL"], mask=1, pos=5)  # I2S_SCLK_OEN
```

Here `mask=1` is an unshifted single-bit field mask; `pos` supplies the bit
position. The other five fields use positions 4 through 0.

The receiver returned `$00`. An initial read failed with firmware status 8;
a repeated read succeeded. No write or pin measurement was performed.
In particular, **the polarity of `OEN` is unverified**: the name could denote an
active-low output enable. The name `IE` suggests input enable, but its electrical
behavior was not tested either. A zero readback does not prove these controls
are implemented or writable on this chip revision.

The older user-supplied structure diagram suggests analogue Q outputs shared
with the digital data and clock pads. Disabling digital drivers might be a
prerequisite for that mode, but **this register does not establish an analogue
signal-routing mechanism**. No analogue-output enable sequence was found.

## $05 bit 0 — FREF_IRQ_MASK

**Evidence: exact software definition; hardware register readback only.** UM
page 11 defines `IRQ_MASK` with reset value `$07`, while saying bit 0 should be
zero. ES supplies the missing field name:

| Register | Bit | Mask | ES field name |
|---|---:|---:|---|
| `$05` / `IRQ_MASK` | 0 | `0x01` | `FREF_IRQ_MASK` |

ES defines this as `RegField(["IRQ_MASK"], 1, 0)`. The receiver readback was
`$07`, so the bit was set at the time of observation.

The name suggests a **reference-frequency interrupt mask**. UM describes the
other IRQ mask fields as 0 = ignored, 1 = allowed to affect IRQ. Applying that
convention to bit 0 is an inference, not a verified description of its polarity
or trigger condition. Neither an IRQ-pin measurement nor a reference-failure
test was performed. There is no evidence that this bit selects analogue outputs
or the external PLL loop filter.

## $C8 bits 7–6 — AFC prefilter controls with incomplete documentation

**Evidence: software definitions and labels already present in the manual.**
These are not wholly undocumented: UM page 76 names them in the
`DPB_AFC_CTL_1` bit diagram, but its explanatory text starts at bits 5–4 and
does not explain the prefilter fields.

| Bit | Mask | ES field name | Interpretation from the name |
|---:|---:|---|---|
| 7 | `0x80` | `DPB_AFC_PREFLT_EN` | AFC prefilter enable |
| 6 | `0x40` | `DPB_AFC_PREFLT_MD` | AFC prefilter mode selection |

ES confirms both as single-bit fields at those positions. ES and UM both give
the register reset value as `$03`, leaving both fields clear. The software field
records provide no named mode choices. The filter response, mode meanings and
enable polarity remain unverified; these bits were not exercised on hardware.

## $B7–$B8 — an opaque combined software field

ES exposes `DPB_CTRLS` across `DPB_CTRLS_1` (`$B7`, upper byte) and
`DPB_CTRLS_0` (`$B8`, lower byte), with unshifted mask `0xFFFF` and position 0.
UM page 68 already describes most individual bits. It leaves `$B7` bit 6
unused and `$B8` bit 1 reserved.

The whole-word software field includes those bits, but supplies **no additional
names or meanings for them**. Its presence is not evidence that they enable a
hidden feature. The documented clock-polarity and I/Q-correction controls in
these registers should not be counted as newly discovered functions.

## Observations without an identified function

The following earlier reads are recorded as observations, not as register
definitions or reset values. They were taken while the firmware had configured
the receiver; nonzero data could represent internal state, trim values or other
implementation details.

| Register(s) | Observed value(s) | What remains unknown |
|---|---|---|
| `$06` / `STATUS` | `$39` | Bit 5 was set, although UM page 12 calls it unused. ES supplies no field name for it. This register is read-only. |
| `$00`, `$01` | `$E5`, `$A0` | UM calls both unused; no identification or revision meaning was established. |
| `$85` | `$C0` | Function unknown; UM page 55 calls it unused. |
| `$86`–`$89` | `$40` each | Functions unknown; UM page 55 calls them unused. |
| `$8A`, `$8B`, `$8C`, `$8D` | `$1F`, `$03`, `$F9`, `$45` | Functions unknown; UM page 55 calls them unused. |
| `$A4`, `$A5` | `$CC`, `$0C` | Functions unknown; UM page 66 calls them unused. |
| `$B3`, `$B4`, `$B6` | `$80`, `$80`, `$01` | Functions unknown; UM page 67 calls them unused. |

No bit meanings are assigned to these values. In particular, `$06` bit 5 is not
assumed to be the status counterpart of `FREF_IRQ_MASK` merely because that
would be plausible.

## Name differences that are not undocumented features

| ES name | Register / bits | Existing UM description |
|---|---|---|
| `RF_LNA_DC_FEED` | `$36`, bit 0 | `LF_LNA_DC_FEED`: LF/MF LNA internal DC bias, page 34 |
| `DPB_DNCONV_MAN` | `$BA`, bit 4 | `DPB_DCONV_MAN`: manual digital-mixer frequency selection, page 70 |
| `DPB_DNCONV_ON` | `$BA`, bit 3 | `DPB_DCONV_ON`: digital mixer enable, page 70 |
| `PLL_SAS_ICI` | `$96`, bit 6 | VCO sub-band-selection infinite-cycle status, page 60 |

These spelling or presentation differences must not be treated as evidence of
extra controls. No additional named register absent from UM was found in the
examined ES tables beyond `I2S_PAD_CTL`.

## Reading from Astra918

From the firmware repository root, with I/Q streaming stopped:

```bash
python3 scripts/register-read.py 0xDA
python3 scripts/register-read.py 0x05
python3 scripts/register-read.py 0xC8
```

The helper prints hexadecimal and binary values. Add `--serial SERIAL` when
multiple receivers are connected. These are direct chip register reads, not
saved receiver settings. Firmware rejects raw register access while I/Q is
streaming. A failed read is not evidence that a register is absent.

No flash Save, reset or firmware change is needed to inspect these registers.
The pilot settings above describe a user-reported measurement; the other
undocumented controls still have no verified write sequence. Further pin
measurements or additional register definitions would clarify their behavior.

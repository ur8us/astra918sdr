# Offline validation — 2026-09-21

All reference repositories remained unchanged. Testing used loopback TCP,
new PTYs and synthetic digital-mode sample files; no receiver, probe, RF
generator or transmitter was accessed. This is a software acceptance record,
not RF, USB bus or real-time MCU qualification.

## Executed successfully

| Area | Evidence |
|---|---|
| Firmware/shared model | 51 Rust unit tests plus one composite USB descriptor/lifecycle test |
| DSP | USB and LSB, offsets −45/0/+45 kHz, residual tuning correction; sideband/alias rejection tests, adjustable passband and reset |
| Composite USB | Five interfaces, three functions, unique endpoints, descriptor memory budget, 12 kHz UAC rate requests, WinUSB vendor binding descriptors, audio lifecycle |
| RP2350 | Both `rp235xa` and `rp235xb` release ELF/UF2 build and picotool file inspection; no flash operation |
| Cross-process receiver | Seven Python acceptance cases: bidirectional CAT/vendor tune, offset invariants, fragmented commands, I/Q metadata/retune, audio after I/Q stop, one vendor owner, configuration fault/retry, save/restart/torn-slot fallback, C++ client and SDR++ lifecycle |
| SDR++ library/module | Linux CMake/CTest, module load against the matching core, real VFO and stream lifecycle test; designated VFO tunes CAT, CAT moves spectrum, secondary VFO stays independent including out-of-span recenter attempts, stop/restart/reconnect |
| Sanitizers | Address/undefined-behavior CTest; module lifecycle passes with leak detection disabled for upstream global DSP allocations |
| SDR++ graphical run | Isolated profile loads Astra source, Radio and Audio Sink; connects, plays and displays the simulated spectrum; audio sink muted |
| Rust GUI | Native Linux release build, draft-preservation unit test, real window/connection smoke; light UI and measured content height inspected |
| Windows | GNU cross-build of portable C++ library/tests and Rust host/GUI; Wine runs C++ protocol and simulator tests, Rust CLI status and GUI connection smoke successfully |
| Digital-mode decoder | Installed WSJT-X 2.7.0 `jt9`/`wsprd` decode FT8, FT4 and WSPR after the production DSP in both USB/+45 kHz and LSB/−45 kHz; message `K1ABC FN42` |
| Live WSJT-X | Installed application 2.7.0 with isolated XDG profile, simulator PTY, UTC-aligned simulated I/Q through production DSP and temporary PipeWire audio: FT8 and FT4 UDP decode reports; WSPR `ALL_WSPR.TXT` and GUI show `K1ABC FN42 33` |
| Live CAT interaction | Actual WSJT-X band dropdown selected 7.074 MHz and receiver state followed; a vendor change to 14.074049 MHz propagated back into WSJT-X’s UDP status. Hamlib’s FW query now reads actual audio width |

The strong noiseless sample files are deterministic functional checks. Decode
counts/SNR values are not sensitivity measurements. The live WSPR run also
overlapped a simulator retune during the module test; a successful decode does
not imply uninterrupted reception across tuning changes.

Reproducible commands are in the three READMEs. `scripts/test_weak_signals.py`
uses external WSJT-X v2.7.0 encoder sources (upstream commit
`b4f9a431bcf6449df8f37b56de79d48b665b044c`), while both decoding paths use the
installed WSJT-X executables. `scripts/live_wsjtx.py --verify` creates a fresh
profile per run and exits after decoding and observing an external CAT retune.

Local evidence is in ignored `artifacts/`: `weak-signals/` decoder outputs,
`wsjtx-live-FT8/udp.log`, `wsjtx-live-FT4/udp.log`,
`wsjtx-live-WSPR/data/WSJT-X - AstraOffline/ALL_WSPR.TXT`, later fresh live-test
directories, and GUI/SDR++/WSPR screenshots. Build outputs:

* `artifacts/rp235xa/astra918.uf2` and `artifacts/rp235xb/astra918.uf2`
* `target/release/astra918-sim`, `target/release/astra918ctl`
* `../astra918sdr-sdrpp/build/astra918_source.so`
* `../astra918sdr-gui/target/release/astra918-gui`
* Windows host/GUI executables under each Rust repo’s
  `target/x86_64-pc-windows-gnu/release/`; C++ executables in `build-win/`.

## Remaining platform and hardware work

macOS execution and native Windows/macOS SDR++ module loading were not available
in this Linux workspace. Portable source, platform-specific socket handling,
build instructions, three-OS CI definitions and a matching-SDK module workflow
are included. No remote CI runs or native USB-driver claims are represented as
completed. Wine is not a substitute for Windows USB/audio qualification.

When the receiver is connected in the later session, confirm board variant,
4 MiB flash and pinout, then test the following **without a frequency generator**:

1. Enumerate audio, CDC and vendor interfaces on each target OS; verify only
   interface 4 is claimed by the controller and that class drivers remain bound.
2. Run WSJT-X recording and CAT simultaneously with SDR++ I/Q. Check real MCU
   DSP timing, DMA overrun protection, USB bandwidth and audio clock drift over
   sustained reception. Verify counters remain stable. Offline tests cannot
   establish this real-time budget.
3. Tune from both applications, change antenna/gain/filter/offset, stop and
   reconnect the I/Q controller, suspend/resume USB, and verify stale audio/IQ
   is discarded and the applied dial remains consistent.
4. Save explicitly, power-cycle, restore settings; test interrupted save and
   confirm fallback to the previous valid slot. Confirm flash layout first.
5. Receive on-air FT8/FT4/WSPR using an antenna when signals are available.
   Assess actual usable spectrum edges and demodulation, with no signal-generator
   or quantitative sensitivity/calibration tests.

No firmware image has been hardware-qualified by the earlier reference
projects’ results. The combined workload and composite USB device are new.

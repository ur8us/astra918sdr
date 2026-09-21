# Approved implementation

Three independent repositories: Astra918 firmware/simulator, SDR++ source,
and Rust light-theme control GUI. Offline implementation and validation first;
receiver tests later, with no frequency generator tests.

One composite USB device exposes UAC1 12 kHz mono PCM, CDC CAT and 120 ksps
ci16 I/Q plus vendor controls. Shared receiver state owns tuning, RF controls,
audio filtering and explicit persistent Save. Dial - offset = spectrum center.
Startup defaults: 14.2 MHz USB, zero offset, 100..3500 Hz audio, AUTO RF input
and RF/IF AGC, unless an explicit saved configuration exists.

CAT and vendor setters are serialized, field-specific and atomic. Clients adopt
state on connect. Settings revisions and I/Q configuration metadata support
external retunes. I/Q stop/disconnect does not stop CAT/audio.

SDR++ links one designated Radio VFO; other VFOs and SDR++ listening modes
remain independent. GUI contains controls/status only, fits its contents,
and keeps USB I/O off the rendering thread. Host targets: Linux/Windows/macOS.

Shared simulator uses firmware state/protocol/DSP, offers vendor TCP and CAT,
and produces synthetic I/Q/audio. Test real WSJT-X in isolated configuration
with temporary virtual audio. Tests distinguish software from hardware proof.

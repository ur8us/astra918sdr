# WSJT-X CAT startup and SDR Console center — 2026-09-25

The connected RP2350A receiver was programmed in BOOTSEL mode with
`picotool load -u -v artifacts/rp235xa/astra918.uf2`. The UF2 occupied
`0x10000000..0x10020e00`, below the settings area starting at `0x103fe000`.
Picotool verified the image. The two saved-settings sectors were backed up
before and read back after programming; both 8192-byte images have SHA-256
`6f2024065ceb7f5760976eed507a7264ba760c9614e75cc655a64554f4d84fc7`.
The receiver's unsaved 18.635 MHz dial was reapplied after reboot; no Save
command was sent. No frequency generator was used.

The installed `/usr/bin/wsjtx` 2.7.0 was launched with an isolated settings
profile using the same Kenwood TS-570D driver as the user's normal profile.
The executable was neither rebuilt nor modified. Before programming, an
isolated TS-480 profile took about 7.1 seconds to report a nonzero dial;
the TS-570D CAT trace showed repeated `?;` replies to `PS`, `KS`, `FB`, `FT`
and `SL`, each followed by Hamlib retry delays. After programming, physical
CAT queries for those commands returned valid responses immediately, and
the TS-570D profile reported the 18.635 MHz dial about 1.7 seconds after
process launch. Its temporary settings profile and logs are under ignored
`artifacts/cat-startup-check/`.

With SDR Console v3.4 under Wine streaming at 120 kS/s, a physical CAT
retune from 18.635 to 18.640 MHz reached the source DLL callback. The main
waterfall frequency scale moved so 18.640 MHz was at its center, then moved
back when the dial was restored. The left-hand RX1 frequency stayed at its
independent, previously selected value. This test confirms source/waterfall
centering, not automatic tuning of SDR Console's local RX1 listener.

# Astra918

Rust/Embassy RP2350 firmware, shared receiver model and offline simulator.
Keep ../cmx918sdr, ../cmx918audiocat and other reference repositories read-only.
No hardware access until the user connects and requests receiver testing.
No frequency-generator tests. Preserve MIT provenance.
Use apply_patch for manual edits. Keep protocol, DSP, hardware and host I/O separate.
CAT dial minus channel offset is the spectrum center. CAT/source-panel/GUI dial
tuning preserves offset; offset edits preserve dial. SDR++ left-right tuning
holds the RF center while moving the shared channel; center tuning uses zero
offset. Firmware owns state; clients adopt it on connection.
Audio/CAT must survive I/Q stop and stalls. Only explicit Save writes settings.
Run formatting, host tests and RP2350 builds; commit coherent milestones.
Document unrun platform/hardware checks accurately. Do not push unless requested.

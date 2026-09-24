# Astra918

Rust/Embassy RP2350 firmware, shared receiver model and offline simulator.
Keep ../cmx918sdr, ../cmx918audiocat and other reference repositories read-only.
No hardware access until the user connects and requests receiver testing.
No frequency-generator tests. Preserve MIT provenance.
Use apply_patch for manual edits. Keep protocol, DSP, hardware and host I/O separate.
CAT dial minus firmware audio offset is the spectrum center. CAT tuning preserves
offset. SDR++/GUI center tuning preserves offset; their audio offset edits use
command 38 to preserve center. SDR++ Radio VFOs are independent local listeners.
Legacy command 33 preserves dial. Firmware owns state; clients adopt it on connection.
Audio/CAT must survive I/Q stop and stalls. Only explicit Save writes settings.
Run formatting, host tests and RP2350 builds; commit coherent milestones.
Document unrun platform/hardware checks accurately. Do not push unless requested.

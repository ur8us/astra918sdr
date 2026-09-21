# Provenance

Original Astra additions are MIT. Incorporated MIT code retains LICENSE notices.
Source repositories are not modified and their histories/artifacts are not copied.

* cmx918sdr: `1efa43e831227a86c443cdecc4895af8ab2b619b`:
  CMX918 driver, RF controls, wide FIR tables/design, recovery, PIO/DMA capture,
  toolchain, linker/build scaffold and host-testable legacy codecs.
* cmx918audiocat: `8fc97d995afd041ae3c847ce4199087fa525689b`:
  CAT parser/replies, audio packetizer, UAC1 recording descriptors and NCO table.
* cmx918_sdrpp_source: `836b9bed51f73c83bcf63bbb268ef56252e1554e`:
  C++ portable library and source-module reference in the sibling repository.
* drm1000-gui: `0816a8a6160f1d2497aa2e91a244f11f93d929b9`:
  GUI architecture reference; its DRM1000 serial protocol is not reused.

CMX918 register references remain in inherited source comments: DS D/918/2.0
and UM918/2.0. Vendor PDFs and RF captures are not redistributed.
Prior projects' hardware results do not qualify Astra's combined firmware.

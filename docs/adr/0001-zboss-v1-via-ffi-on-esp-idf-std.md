# Zigbee via Espressif's ZBOSS v1 through FFI, not a pure-Rust stack

The Rust firmware runs on ESP-IDF `std` (`esp-idf-svc`) and drives Espressif's closed-source ZBOSS
stack (`esp-zigbee-lib ==1.6.8`, `esp-zboss-lib ==1.6.4`) through bindgen, with all `unsafe` kept in
`firmware/src/zb.rs`. The pure-Rust `zigbee-rs` (0.1, 2026) lacks the CO2 and On/Off clusters. The
ZBOSS v2 SDK has a new API and almost certainly a different storage format, which would force a
re-pair. Staying on v1 keeps the existing pairing in `zb_storage` and copies proven patterns from
existing Rust projects (ManfredFr/CO2-Sensor-Zigbee-Rust, canack/huesmith). Revisit when a Rust stack
covers CO2 + On/Off, or when v1 stops receiving fixes.

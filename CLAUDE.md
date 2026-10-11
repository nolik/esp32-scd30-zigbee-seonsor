# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

ESP32-C6 + Sensirion SCD30 Zigbee router (CO2 / temperature / humidity) in Rust, paired with
Zigbee2MQTT + Home Assistant. Domain terms (Self-Heal, Reference Recalibration, FRC, Stored
Reference, Trusted Session, Post-FRC Transient, Measurement Interval, Report Period) are defined in
`CONTEXT.md` — use them in code, docs and commit messages.

## Commands

Logic (host, any machine, plain `cargo`):
```
cd logic && cargo test                              # all tests
cd logic && cargo test reference_recalibration      # tests matching a name
```

Firmware builds, flashing and the device all live on the NixOS build host
`ssh nolik@192.168.10.200`, repo `/home/nolik/esp32-scd30-zigbee-seonsor`, inside the pinned
dev shell (`flake.nix`: Rust nightly, ESP-IDF 5.3.3, espflash, ldproxy, mosquitto, jq):
```
nix --extra-experimental-features 'nix-command flakes' develop -c sh -c 'cd firmware && cargo build --release'
nix --extra-experimental-features 'nix-command flakes' develop -c ./scripts/flash.sh            # bootloader + table + app
./scripts/z2m-contract.sh --reinterview   # Z2M device entry; diff against docs/z2m-baseline.json
```
The host's checkout has its branch checked out, so sync by pushing to a side ref and fast-forwarding
there: `git push -f ssh://nolik@192.168.10.200/home/nolik/esp32-scd30-zigbee-seonsor HEAD:refs/heads/incoming`
then `git merge --ff-only incoming` on the host. Serial logs: `espflash monitor` needs a TTY; over
ssh use a small pyserial reader in `nix shell` (opening the port can reset the ESP, not the SCD30).

## Architecture

- `logic/` — hardware-free crate holding all behaviour: `calib.rs` (intervals, FRC/plausibility
  rules), `zcl.rs` (typed, clamped ZCL values), `sensor/` (`SensorService` state machine: boot
  ensure-settings, retry/bus recovery, Self-Heal, Reference Recalibration, Stored Reference saving).
  Hardware sits behind four traits (`Scd30`, `StoredReference`, `Reporter`, `Clock`); tests use
  recording fakes (`sensor/fakes.rs`) and assert call order.
- `firmware/` — ESP-IDF `std` binary implementing those traits: `sensor_hw.rs` (libscd over I2C),
  `store.rs` (NVS), `clock.rs`, and `zb.rs` — the only module touching the ZBOSS C API (bindgen
  bindings at `esp_idf_svc::sys::zb`, built from `src/zb/bindings.h`). All `unsafe` lives there.
- Threads: `zb::start` runs the ZBOSS loop on its own thread; one sensor thread is the sole I2C
  owner, started once in `main`; Zigbee's "Trigger Recalibration" switch reaches it over `mpsc`.
- The crates are siblings (not a Cargo workspace): `firmware/.cargo/config.toml` forces the ESP
  target, so `logic` must stay buildable for the host.
- Why it is built this way: `docs/rust-migration.md` (decision record, parity spec, soak log) and
  `docs/adr/` (ZBOSS v1 via FFI, partition layout, Self-Heal 15 s vs Reference Recalibration 2 s).
  Why Self-Heal exists at all: `docs/scd30-defect-evidence.md` (this SCD30 loses FRC on power loss).

## Hard constraints

- Preserve the device's state across flashes: flash only with `scripts/flash.sh` (no erase), keep
  `firmware/partitions.csv` offsets (`nvs`, `zb_storage`, `zb_fct`), keep NVS keys `scd30_cal`/`last_co2`
  (i32), and keep `EspDefaultNvsPartition::take_with(false)`. Losing them means a Zigbee re-pair and
  a Reference Recalibration. The one deliberate exception: a Zigbee role change clears only
  `zb_storage` once (README "Changing the Zigbee role").
- The ESP32 port is `/dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_A0:F2:62:45:CE:34-if00`;
  `/dev/ttyUSB0` on the host is the Zigbee2MQTT coordinator — leave it alone. After a replug the
  port needs `sudo chmod 666 /dev/ttyACM*` (user action).
- The Zigbee identity is a frozen contract with `esp32c6_scd30_converter.js` and
  `docs/z2m-baseline.json` (manufacturer `Nolik`, model `ESP32C6-SCD30-Zigbee`, endpoint 1, clusters,
  role router - ADR 0004: as an end device its plug parent dropped it every 64 min).
- Self-Heal runs at the 15 s Measurement Interval; only Reference Recalibration switches to 2 s
  (ADR 0003 — an interval change shifts CO2 ~7 ppm on this unit).
- ZBOSS is pinned (`esp-zigbee-lib ==1.6.8`, `esp-zboss-lib ==1.6.4`); `firmware/Cargo.lock` and
  `firmware/components_esp32c6.lock` are the device-verified versions.
- Ask before flashing firmware or triggering a Reference Recalibration (it recalibrates the real sensor).

## Gotchas

- I2C needs `.timeout(100 ms)` in `sensor_hw.rs`: SCD30 clock stretching exceeds the IDF default
  (symptom: `ESP_ERR_TIMEOUT` on reads).
- `CONFIG_PARTITION_TABLE_CUSTOM_FILENAME` is `../../../../../../partitions.csv` because esp-idf-sys
  resolves it from `target/<triple>/<profile>/build/esp-idf-sys-*/out`.
- bindgen names enum constants `<c_type>_<NAME>`; the action callback is
  `esp_zb_core_action_callback_id_s_ESP_ZB_CORE_SET_ATTR_VALUE_CB_ID` (`_s_`).
- Z2M caches interviews; re-interview before judging the contract. `power_source` shows `null` for
  both the Rust and the old C++ firmware.
- The previous C++ firmware is tag `cpp-final`; README "Rollback to the C++ firmware" has the steps
  (never `idf.py set-target` there — it drops the custom partition table).

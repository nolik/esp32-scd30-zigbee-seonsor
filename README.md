# ESP32-C6 + SCD30 Zigbee CO2/Temperature/Humidity Sensor

> **Repository layout:** `firmware/` (ESP-IDF `std` Rust, Zigbee via ZBOSS FFI) + `logic/`
> (hardware-free, host-tested) are the firmware. The previous C++ firmware lives in git history at
> tag `cpp-final` (see Rollback below). Glossary: `CONTEXT.md`; decisions:
> `docs/rust-migration.md`, `docs/adr/`.

DIY Zigbee router built on ESP-IDF, exposing CO2 (SCD30, NDIR), temperature,
humidity, and a "Trigger Recalibration" switch to Zigbee2MQTT / Home Assistant.

## Hardware

- ESP32-C6 (Zigbee + native 802.15.4 radio)
- Sensirion SCD30 (I2C, address `0x61`)
- Wiring: SDA -> `GPIO21`, SCL -> `GPIO7`
  - **Do not use `GPIO0` for SCL** - it's a strapping pin on the C6 and can
    interfere with boot / get corrupted by RF activity from the radio keying up.

---

## 0. Environment (NixOS build host)

Everything (Rust nightly, ESP-IDF v5.3.3, `espflash`, `ldproxy`, `mosquitto`, `jq`) is pinned in
`flake.nix`/`flake.lock`:
```
nix --experimental-features 'nix-command flakes' develop
```
The ESP32-C6 Each re-plug resets its permissions: `sudo chmod 666 /dev/ttyACM*`, or
add your user to the `dialout` group. **Never point tools at `/dev/ttyUSB0` - that is the
Zigbee2MQTT coordinator.**

---

## 1. Build

```
cd firmware && cargo build --release     # first build compiles ESP-IDF (~10+ min)
cd logic && cargo test                    # host tests for the sensor logic (any machine)
```

---

## 2. Partition table

`firmware/partitions.csv` keeps the C++ firmware's data-partition offsets so the Zigbee pairing
(`zb_storage`) and the Stored Reference (`nvs`) survive firmware swaps; the app moved to
`factory @ 0x120000` (2 MB) and the old 1 MB app area is deliberately unused
([ADR 0002](docs/adr/0002-partition-layout-keeps-old-offsets.md)). The boot log must list 5
partitions with `factory` at `0x00120000`.

---

## 3. Flash

```
./scripts/flash.sh            # bootloader + partition table + app
./scripts/flash.sh --monitor  # same, then serial monitor (needs an interactive terminal)
```
**Never erase flash** (`espflash erase-flash`, `--erase-parts`): it destroys the pairing and the
Stored Reference.

**Changing the Zigbee role** (end device <-> router, [ADR 0004](docs/adr/0004-zigbee-router-not-end-device.md);
also needed for a rollback to `cpp-final` from a router build) needs a fresh join. Clear only
`zb_storage` once, then flash. `nvs` (Stored Reference) stays, and so does the Z2M entry (same IEEE):
```
cd firmware && espflash erase-parts --chip esp32c6 --partition-table partitions.csv \
  --port /dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_A0:F2:62:45:CE:34-if00 zb_storage
cd .. && ./scripts/flash.sh
```
Z2M must allow joining (`permit_join`); it re-interviews and reconfigures the device on its own.

**Rollback to the C++ firmware** (it reads the same pairing and Stored Reference):
```
git worktree add /tmp/cpp-final cpp-final && cd /tmp/cpp-final
nix --experimental-features 'nix-command flakes' develop github:mirrexagon/nixpkgs-esp-dev/5287d6e1ca9e15ebd5113c41b9590c468e1e001b#esp32c6-idf
idf.py -p /dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_A0:F2:62:45:CE:34-if00 flash
```
Do **not** run `idf.py set-target` there: it regenerates `sdkconfig` and drops the custom
partition table (the device then boot-loops on a missing `zb_storage`).

---

## 4. Calibration: Self-Heal and Reference Recalibration

Terms are defined in [`CONTEXT.md`](CONTEXT.md).

**This SCD30 unit has a confirmed hardware defect** (bought on AliExpress, authenticity
unverified): it loses its FRC calibration on every power cycle, even though Sensirion's datasheet
says it shouldn't, and then reads ~1 ppm. Verified with controlled power-cut tests on 2026-10-10
(ASC off, Sensirion's 2 s FRC procedure, every other sensor setting persisting) - see
[`docs/scd30-defect-evidence.md`](docs/scd30-defect-evidence.md).

The firmware compensates with **Self-Heal**: the last trusted CO2 reading is kept as the **Stored
Reference** in ESP32 NVS (saved every ~5 min during a Trusted Session) and re-applied via FRC two
minutes after every boot; readings are not reported until then. With a good replacement sensor,
build with `--no-default-features` to disable Self-Heal.

A fresh board (empty NVS) needs one **Reference Recalibration**: put the sensor at an open window
(fresh outdoor air, still in Zigbee range) for a few minutes and flip the **Trigger Recalibration**
switch in Home Assistant/Zigbee2MQTT. The firmware switches the sensor to a 2 s Measurement
Interval, waits 2 minutes, applies FRC = 425 ppm (`FRC_REFERENCE_PPM`, drifts a few ppm per year),
returns to 15 s, saves the Stored Reference and resets the switch to off.

- ASC (automatic self-calibration) is kept **permanently off**: the boot-time FRC always overrides
  it on this unit. Interval (15 s), ASC (off) and temperature offset (320) are read at boot and
  written only if different.
- Self-Heal stays at the 15 s interval while Reference Recalibration uses 2 s: any interval change
  shifts CO2 by ~7 ppm on this unit, and a 2 s Self-Heal would push readings ~7 ppm lower on every
  reboot ([ADR 0003](docs/adr/0003-self-heal-at-15s-reference-recalibration-at-2s.md)). After a
  Reference Recalibration, 15 s readings sit ~7 ppm below 425 (constant, inside the ±30 ppm spec).
- The one ~1 ppm reading the sensor produces right after any FRC (Post-FRC Transient) is discarded.

---

## 5. Zigbee2MQTT external converter (NixOS)

The device model
`ESP32C6-SCD30-Zigbee`. Without the converter, Z2M shows it as "Unsupported" /
"Automatically generated definition" and CO2 will show a fixed wrong value
(the raw ZCL fraction misinterpreted) with temperature/humidity/switch not
working correctly.

The converter file lives in this repo as `esp32c6_scd30_converter.js`.

**Z2M 2.x requires external converters in an `external_converters/`
subfolder inside the data directory - not the data directory root.** On
NixOS with the `services.zigbee2mqtt` module, the default data directory is
`/var/lib/zigbee2mqtt`.

```
sudo mkdir -p /var/lib/zigbee2mqtt/external_converters
sudo cp esp32c6_scd30_converter.js /var/lib/zigbee2mqtt/external_converters/
# Ownership matters for BOTH the file and the directory - Z2M needs to create
# a node_modules symlink inside this folder at startup, which fails with
# EACCES if the directory itself is root-owned (e.g. from `sudo mkdir`):
sudo chown -R zigbee2mqtt:zigbee2mqtt /var/lib/zigbee2mqtt/external_converters
sudo chmod 755 /var/lib/zigbee2mqtt/external_converters
```

In your NixOS configuration:
```nix
services.zigbee2mqtt.settings = {
  external_converters = [ "esp32c6_scd30_converter.js" ];
  # ...your other existing settings...
};
```

```
sudo nixos-rebuild switch
sudo systemctl restart zigbee2mqtt
```

Confirm it loaded cleanly:
```
journalctl -u zigbee2mqtt -n 50 --no-pager | grep -i -A 10 convert
```
No output here is fine (means no error); an explicit `Failed to load external
converter` or `EACCES ... symlink` error means the folder/ownership steps
above weren't applied correctly - fix and restart again.

Occasionally a device's `configure()` (bind + reporting setup) can fail on a
given boot due to ordinary one-shot radio packet loss - see Step 6 for what
that looked like during development and why it's rare now that the real
underlying bug (also Step 6) is fixed.

---

## 6. The temperature-stuck-at-null bug (fixed) and polling (tried, removed)

**This was the real, root-caused bug**, found via Zigbee2MQTT debug-level
logging (`advanced.log_level: debug` + `DEBUG=zigbee-herdsman:*`) capturing
the actual over-the-air frames rather than just summary log lines:

At boot, all four clusters (CO2, temperature, humidity, on/off) successfully
send their *first* attribute report - proving binding/reporting genuinely
works, contrary to what the symptom looked like. CO2 and humidity later send
a **second** report once self-heal completes, correctly reflecting their real
values. **Temperature never sent a second report, ever** - in any capture,
across many reboots.

Root cause: temperature's original placeholder was `(int16_t)0x8000`
(`-32768`) - the ZCL-defined "invalid" sentinel, and also the most extreme
possible value a signed 16-bit number can hold. The jump from that placeholder
to a real reading (e.g. `2073` = 20.73C) is a delta of ~34841, which itself
overflows signed 16-bit arithmetic (max +32767). If the reporting engine's
"has this changed enough to report?" check computes that delta using 16-bit
signed math, the overflow can silently produce a wrong (too-small) result,
permanently convincing it nothing worth reporting ever happened again. CO2
(a float) and humidity (unsigned, whose placeholder sits at a safer point in
its own range) don't hit this edge case. `-32768` was also, on reflection,
outside the cluster's own declared valid range (`-4000` to `8500`) - a second
reason it was a bad choice.

**Fix applied**: temperature's placeholder is now `0` (0.00C) instead of
`0x8000` - safely within the declared range, and no delta calculation
involving it can overflow. Cosmetic tradeoff: during the ~2 minute self-heal
window, temperature briefly shows `0.00C` instead of `null`/unavailable -
minor, and self-corrects automatically once the first real reading lands.

A one-time diagnostic in the C++ firmware (`log_attribute_reporting_flags()` in `main/main.cpp` at tag `cpp-final`,
logged once at boot as `Attr check [...]`) confirmed all four attributes
correctly carry the ZCL `REPORTING` access flag, ruling out a simpler
"attribute not marked reportable" explanation before the real cause was found.

### Polling (tried during development, removed)

A 60-second polling safety net (`onEvent` in the converter) was added while
debugging, as defense against occasional one-shot bind failures (see below).
Once the actual root cause was found and fixed, testing confirmed push-based
reporting alone is reliable, so polling was **removed** to keep the converter
simple - pure `configure()` (bind + reporting) is the current, final setup.

If a future symptom looks like "occasional single-cluster reporting hiccup on
some reboot" again, re-adding an `onEvent` poll loop (reading each cluster's
`measuredValue` every 60s) is a reasonable fallback to reach for - it's cheap,
harmless, and was proven to work during development. Rationale for why it
would help: bind is a **one-shot** ZDO operation (a single request/reply that
never automatically retries if lost), unlike reads, which are **repeating**
(a lost one just retries next cycle with no consequence) - so a bind hiccup
is structurally more fragile than a comparable read/report hiccup, even
though both face the same ordinary radio-level packet loss.

### Manual reconfigure (rarely needed)

To nudge the efficient push path back on for a specific device: MQTT publish
`{"id": "CO2 sensor"}` (or the IEEE address) to
`zigbee2mqtt/bridge/request/device/configure` - this is exactly what the
frontend's Reconfigure button does. Not required for correctness, only for
the (nice-to-have) lower-latency push behavior.

---

## Troubleshooting reference

| Symptom | Cause | Fix |
|---|---|---|
| `zb_assert()` / abort right after boot, boot log doesn't show 5 partitions with `factory` at `0x00120000` | Partition table not flashed | `scripts/flash.sh` passes `--partition-table`; see Step 2 |
| `phy_init: NVS has not been initialized` warning | NVS taken after the Zigbee platform config | `main` takes `EspDefaultNvsPartition` before `zb::start` - keep that order |
| Steering keeps failing (`status: -1` repeatedly) | Channel mask too narrow, or permit join not enabled on the coordinator | Use `ESP_ZB_TRANSCEIVER_ALL_CHANNELS_MASK`; enable permit join in Z2M |
| `clear bus failed` / `reset hardware failed`, especially after long uptime or a Zigbee rejoin | (C++ era) a second I2C owner: a re-fired steering signal spawned a duplicate sensor task | The Rust sensor thread is the only I2C owner, started once in `main`; never start threads from `esp_zb_app_signal_handler` |
| CO2 stuck near 0-1 ppm, temperature/humidity fine, no response to breath | Confirmed hardware defect in this SCD30 unit's calibration storage ([evidence](docs/scd30-defect-evidence.md)) | Self-heal + FRC workaround (Step 4); detector itself works fine once calibrated |
| FRC succeeds (`OK` logged) but reverts after a real power cycle | Same defect as above - contradicts Sensirion's own documented persistence behavior | Self-heal mechanism re-asserts FRC automatically every boot from ESP32 NVS |
| `Discarding Post-FRC Transient` in the log after every FRC | The sensor's first reading after FRC is ~1 ppm garbage | Expected; it is not reported |
| Z2M shows "Unsupported" / "Automatically generated definition" despite installing the converter | Wrong folder (needs `external_converters/` subfolder, not the data dir root) | See Step 5 |
| `EACCES ... symlink 'node_modules'` in Z2M log | The `external_converters/` directory itself is root-owned | `chown -R` the *directory*, not just the `.js` file, to the `zigbee2mqtt` service user |
| Converter loads, device shows correct model, but still says "Unsupported" / fields stuck at placeholder values right after installing the converter | `configure()` hasn't run for this already-paired device yet | Click **Reconfigure** on the device page in Z2M once |
| Temperature specifically stuck at `null` forever after boot, while CO2/humidity update fine | **Fixed** - was a signed 16-bit overflow in the reporting engine's delta check, triggered by the old `-32768` placeholder. See Step 6 | Placeholder is `0` (`logic::zcl::TEMP_PLACEHOLDER`, unit-tested) - don't revert to `0x8000` |
| Any single field occasionally stuck after a specific reboot (rare now that the Step 6 bug is fixed) | A one-shot ZDO Bind Request can occasionally fail on ordinary radio packet loss (confirmed once via `AREQ - ZDO - bindRsp after 10000ms`) | Click Reconfigure once; consider re-adding the `onEvent` polling pattern from Step 6 if this becomes frequent |
| Breath test shows only a small CO2 rise (e.g. 400 -> 560 ppm) | Expected - exhaled breath (~40,000+ ppm) is heavily diluted by room air before reaching the sensor, and the sensor has a ~20s response time | Not a sign of inaccuracy; use FRC against a genuine known reference to judge real accuracy, not a breath test |
| **Rust:** `Failed to read SCD30 measurement: I2C(... ESP_ERR_TIMEOUT ...)` | SCD30 clock stretching exceeds the default I2C bus timeout | `sensor_hw.rs` sets `.timeout(100 ms)` (C6 max ~105 ms) - keep it |
| **Rust:** build fails, `partitions.csv` not found under `target/.../out` | esp-idf-sys resolves the partition-table path from its generated project | `CONFIG_PARTITION_TABLE_CUSTOM_FILENAME="../../../../../../partitions.csv"` in `sdkconfig.defaults` |
| **Rust:** link error `undefined reference to esp_zb_app_signal_handler` | ZBOSS calls an app-defined handler | Defined `#[no_mangle]` in `firmware/src/zb.rs` |
| **Rust:** constant `..._CORE_SET_ATTR_VALUE_CB_ID` not found | bindgen names it after the C struct tag | Use `esp_zb_core_action_callback_id_s_ESP_ZB_CORE_SET_ATTR_VALUE_CB_ID` |
| **Rust:** Z2M still shows old clusters/strings after a firmware change | Interview data is cached | `scripts/z2m-contract.sh --reinterview`, diff against `docs/z2m-baseline.json` (`power_source` shows `null` for both firmwares today) |
| **Rust:** CO2 values are whole ppm, up to 1 ppm low | `libscd` truncates CO2 (`as u16`) | Accepted (Q33); the converter rounds to ppm anyway |

---

## Key constants (`logic/src/calib.rs`, `logic/src/zcl.rs`)

| Constant | Purpose |
|---|---|
| `MEASUREMENT_INTERVAL_S` (15) | SCD30 Measurement Interval and the firmware's Report Period; also used by Self-Heal |
| `FRC_INTERVAL_S` (2) | Measurement Interval for the 2 minutes before a Reference Recalibration |
| `TEMPERATURE_OFFSET_CENTI_C` (320) | Sensor-side self-heating correction (ensured at boot); refine against a reference thermometer: `(SCD30 - reference) * 100 + current` after ~10 min |
| `FRC_REFERENCE_PPM` (425) | Assumed outdoor CO2 for a Reference Recalibration - check periodically |
| `SELF_HEAL_WAIT_MS` (120 000) | Measurements before any FRC (Sensirion: >= 2 min) |
| `SAVE_EVERY_READINGS` (20, ~5 min) | How often a Trusted Session updates the Stored Reference |
| `clamp_frc_reference` (400-2000) / `is_plausible` (300-10000) | FRC command range / sanity bounds for saving and re-applying |
| `TEMP_PLACEHOLDER` (`0`) | Temperature value before the first reading - see Step 6; never `0x8000` |
| Cargo feature `self-heal` (default on) | Disable with `--no-default-features` for a sensor that keeps its FRC |

# ESP32-C6 + SCD30 Zigbee CO2/Temperature/Humidity Sensor

DIY Zigbee end device built on ESP-IDF, exposing CO2 (SCD30, NDIR), temperature,
humidity, and a "Trigger Recalibration" switch to Zigbee2MQTT / Home Assistant.

## Hardware

- ESP32-C6 (Zigbee + native 802.15.4 radio)
- Sensirion SCD30 (I2C, address `0x61`)
- Wiring: SDA -> `GPIO21`, SCL -> `GPIO7`
  - **Do not use `GPIO0` for SCL** - it's a strapping pin on the C6 and can
    interfere with boot / get corrupted by RF activity from the radio keying up.

---

## 0. One-time environment setup (fresh NixOS install)

Check which port the board enumerates as:
```
ls /dev/tty*
```
If it's `/dev/ttyACM0`, fix permissions for this session:
```
sudo chmod 666 /dev/ttyACM0
```
(or add your user to the `dialout` group permanently via NixOS config, which
survives reboots).

Enter the ESP-IDF dev shell (nixpkgs has no plain `esp-idf` package - this uses
the community `nixpkgs-esp-dev` flake, targeting the C6):
```
nix --experimental-features 'nix-command flakes' develop github:mirrexagon/nixpkgs-esp-dev#esp32c6-idf
```
(`esp-idf-full` also works and covers all targets, at the cost of a larger download.)

---

## 1. Build

```
idf.py set-target esp32c6      # only needed once, or after a full clean
idf.py build
```

Watch for these two build-time gotchas (see Troubleshooting if hit):
- `driver/i2c_master.h: No such file or directory` -> missing `driver` in
  `CMakeLists.txt`'s `REQUIRES`.
- `'ESP_ZB_DEFAULT_RADIO_CONFIG' was not declared` / `'DEFINE_PSTRING' was not
  declared` -> these SDK macros aren't auto-included by `esp_zigbee_core.h` in
  this SDK version; already worked around locally in `main.cpp` (see comments
  there) - don't re-add the SDK's own macro definitions on top.

---

## 2. Partition table (required before first flash)

The Zigbee stack needs its own NVM partitions (`zb_storage`, `zb_fct`) beyond
the default table. `partitions.csv` in this repo already defines them:
```
# Name,      Type, SubType, Offset,   Size,  Flags
nvs,         data, nvs,     0x9000,   0x6000,
phy_init,    data, phy,     0xf000,   0x1000,
factory,     app,  factory, 0x10000,  0x100000,
zb_storage,  data, fat,     0x110000, 16K,
zb_fct,      data, fat,     0x114000, 4K,
```
Confirm it's actually wired up: `idf.py menuconfig` -> **Partition Table** ->
must be set to **Custom partition table CSV**, filename `partitions.csv`. If
it shows "Single factory app, no OTA" instead, select the custom option, save,
then `idf.py fullclean && idf.py build`.

You'll know this is wrong if the boot log's partition table only lists 3
entries (`nvs`, `phy_init`, `factory`) instead of 5 - and you'll see a
`zb_assert()` abort shortly after `esp_zb_init()` runs.

---

## 3. Flash

With monitor, for troubleshooting boot/join/sensor issues:
```
idf.py -p /dev/ttyACM0 flash monitor
```
(Ctrl+] to exit the monitor.)

Once things look good, plain flash is fine:
```
idf.py -p /dev/ttyACM0 flash
```

---

## 4. First boot: seed the self-heal calibration reference

**This SCD30 unit has a confirmed hardware defect**: its Forced Recalibration
(FRC) value does not survive a real power cycle, even though Sensirion's own
datasheet says it should. The firmware works around this with a *self-heal*
mechanism (stores the last trusted CO2 reading in the ESP32's own NVS flash,
which - unlike the SCD30's NVM - reliably survives power loss, and reasserts
it via FRC on every boot). But NVS starts empty, so it needs seeding once:

1. In `main.cpp`, set `SCD30_PERFORM_FRC` to `true`. Take the device outside
   (or right by an open window with good airflow).
2. `idf.py build && idf.py -p /dev/ttyACM0 flash monitor` while outdoors. Wait
   for the `Forced recalibration to ... ppm: OK` and `Saved ... ppm as new
   self-heal reference` log lines.
3. Set `SCD30_PERFORM_FRC` back to `false`, reflash. From here on, every boot
   self-heals automatically - no more manual outdoor trips needed for normal
   operation.

`SCD30_FRC_REFERENCE_PPM` (currently `425`) is the assumed outdoor CO2
baseline - this drifts a few ppm per year (was ~420 a couple years ago); worth
a quick check if redoing this far in the future.

`SCD30_ENABLE_ASC` automatically follows `SCD30_PERFORM_FRC` (off during a
manual FRC run to avoid the two conflicting, on otherwise as a background
safety net) - nothing to configure separately.

You can also trigger a fresh FRC any time from Home Assistant/Zigbee2MQTT
(see below) instead of editing/reflashing - flip the exposed **Trigger
Recalibration** switch on while the sensor is exposed to a known reference; it
resets itself off automatically once done.

---

## 5. Zigbee2MQTT external converter (NixOS)

The device identifies itself as manufacturer `Nolik`, model
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

**After the converter loads successfully, still click "Reconfigure" on the
device's page in the Z2M frontend once.** Loading the converter alone doesn't
re-run its `configure()` step (which sets up the actual attribute
binding/reporting) on an already-paired device - only a fresh pairing or a
manual Reconfigure does. Skipping this shows some fields (e.g. temperature,
linkquality) as stuck/`null` even though the converter is technically active.

---

## Troubleshooting reference

| Symptom | Cause | Fix |
|---|---|---|
| `driver/i2c_master.h: No such file or directory` | Missing `driver` in `CMakeLists.txt` `REQUIRES` | Add `driver` to the `REQUIRES` list |
| `'ESP_ZB_DEFAULT_RADIO_CONFIG' was not declared` | Not auto-included by `esp_zigbee_core.h` in this SDK version | Already defined locally in `main.cpp` - don't remove |
| `'DEFINE_PSTRING' was not declared` | Same as above, lives in a header not pulled in | Manufacturer/model strings are built as raw Pascal-string byte arrays instead - see `main.cpp` |
| Narrowing conversion errors on cluster `measured_value` | Different clusters use different signedness (temperature = `int16_t`, humidity = `uint16_t`) per ZCL spec | Match the cast to each cluster's actual field type |
| `zb_assert()` / abort right after boot, partition table shows only 3 entries | Custom partition table not actually active | See Step 2 above |
| `phy_init: NVS has not been initialized` warning | Missing `nvs_flash_init()` | Already called in `app_main()` - don't remove |
| Steering keeps failing (`status: -1` repeatedly) | Channel mask too narrow, or permit join not enabled on the coordinator | Use `ESP_ZB_TRANSCEIVER_ALL_CHANNELS_MASK`; enable permit join in Z2M |
| `clear bus failed` / `reset hardware failed`, especially after long uptime or a Zigbee rejoin | Duplicate `SCD30_Task` spawned by a re-fired `ESP_ZB_BDB_SIGNAL_STEERING` (happens on any rejoin, not just first boot), causing two tasks to fight over the same I2C bus | Already guarded with a `static bool scd30_task_started` flag in `esp_zb_app_signal_handler` - don't remove |
| CO2 stuck near 0-1 ppm, temperature/humidity fine, no response to breath | Confirmed hardware defect in this SCD30 unit's calibration storage (see dispute report if filed) | Self-heal + FRC workaround (Step 4); detector itself works fine once calibrated |
| FRC succeeds (`OK` logged) but reverts after a real power cycle | Same defect as above - contradicts Sensirion's own documented persistence behavior | Self-heal mechanism re-asserts FRC automatically every boot from ESP32 NVS |
| Z2M shows "Unsupported" / "Automatically generated definition" despite installing the converter | Wrong folder (needs `external_converters/` subfolder, not the data dir root) | See Step 5 |
| `EACCES ... symlink 'node_modules'` in Z2M log | The `external_converters/` directory itself is root-owned | `chown -R` the *directory*, not just the `.js` file, to the `zigbee2mqtt` service user |
| Converter loads, device shows correct model, but still says "Unsupported" / fields stuck at placeholder values (co2: 400, temperature: null) | `configure()` hasn't run for this already-paired device yet | Click **Reconfigure** on the device page in Z2M once |
| Breath test shows only a small CO2 rise (e.g. 400 -> 560 ppm) | Expected - exhaled breath (~40,000+ ppm) is heavily diluted by room air before reaching the sensor, and the sensor has a ~20s response time | Not a sign of inaccuracy; use FRC against a genuine known reference to judge real accuracy, not a breath test |

---

## Key firmware constants (`main.cpp`)

| Constant | Purpose |
|---|---|
| `SCD30_MEASUREMENT_INTERVAL_SECONDS` (15) | SCD30 internal measurement + reporting poll interval |
| `SCD30_PERFORM_FRC` | Set `true` once, outdoors, to (re)seed calibration; `false` for normal operation |
| `SCD30_FRC_REFERENCE_PPM` (425) | Assumed outdoor CO2 ppm for manual FRC - check/update periodically |
| `SCD30_ENABLE_ASC` | Auto-derived as `!SCD30_PERFORM_FRC` - don't set directly |
| `SCD30_FRC_MIN_PPM` / `MAX_PPM` (400-2000) | FRC command's own documented valid input range |
| `SCD30_PLAUSIBLE_MIN_PPM` / `MAX_PPM` (300-10000) | Sanity bounds for self-heal save/reassert, matching the sensor's accuracy-guaranteed range |
| `SCD30_SAVE_INTERVAL_READINGS` (20, ~5 min) | How often a trusted live reading updates the self-heal reference in NVS |

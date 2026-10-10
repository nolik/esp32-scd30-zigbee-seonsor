# Rust migration — decision record

Status: **accepted** (2026-10-10). Outcome of a design grilling session; every
decision below was explicitly agreed. Rollback target: tag `cpp-final`.

## Why (and why not)

Goal is mainly **learning Rust on embedded**, secondarily correctness. Honest
assessment: of the four real bugs this project hit, only one (two tasks fighting
over the I2C bus) is a class Rust prevents. The temperature-null bug lived inside
closed-source ZBOSS, the FRC-not-persisting issue is an SCD30 hardware defect, and
bind failures are radio packet loss. The rewrite is worth it as a project with a
known path (near-twin reference projects exist), not as a production fix.

The C++ original is effectively C in a `.cpp` file (only `std::function` + lambdas
for `scd30_retry_until_ok`, one range-for, `extern "C" app_main`). Several README
gotchas (narrowing errors, redefined `ESP_ZB_*_CONFIG()` macros) are C++-specific
and do not carry over.

## Decisions

| # | Decision | Why |
|---|---|---|
| Q1 | Rewrite in Rust; `cpp-final` tag is the rollback | Learning + typed safety; rollback keeps it low-risk |
| Q2 | Preserve Zigbee pairing (`zb_storage`) **and** NVS self-heal reference | Re-seeding needs a manual outdoor FRC run |
| Q3 | Frozen Z2M contract — `esp32c6_scd30_converter.js` untouched | Changing both sides doubles the Zigbee debug space |
| Q4 | Behavior parity (see below, as amended by Q29/Q30), **except** drop compile-time `SCD30_PERFORM_FRC` | The Zigbee recalibration switch already covers seeding |
| Q5 | Done = parity + 7-day soak, then delete C++ | Previous bugs only surfaced over long uptime |
| Q6 | Staged big-bang rewrite: sensor first, then Zigbee | 678 LOC — strangler-fig overhead not worth it |
| Q7 | Same repo: C++ → `legacy-cpp/`, Rust at root, tag `cpp-final`; delete `legacy-cpp/` after soak | Both buildable during soak |
| Q8 | Host `cargo test` for pure logic; SCD30 driver tested with `embedded-hal-mock` | Targets the bug classes actually hit |
| Q9 | Drop PlatformIO | No Rust support; `cargo` + `espflash` |
| Q10 | Pinned `flake.nix` + `flake.lock` + `rust-toolchain.toml` | ESP Rust builds are version-sensitive |
| Q11 | `std` route: `esp-idf-svc` + Espressif ZBOSS SDK **v1**, pinned `esp-zigbee-lib ==1.6.8`, `esp-zboss-lib ==1.6.4` | v2 = new API + different storage format (breaks Q2); `zigbee-rs` lacks CO2/OnOff clusters |
| Q12 | ESP-IDF **5.3.3** (production C++ is on 5.5.2) | Proven by Rust reference projects; NVS/ZBOSS storage formats are IDF-minor independent |
| Q13 | Offset-preserving partition layout, `factory` moved + enlarged to 2 MB, no OTA | Keeps pairing + NVS; flash measured at 4 MB |
| Q14 | bindgen via `extra_components` + one hand-written safe `zb` module; per-cluster value newtypes | All `unsafe` in one file; wrong-cast bugs become compile errors |
| Q15 | `libscd` 0.5.1 (sync) | FRC / ASC / interval confirmed in source; swap-able behind `embedded-hal` |
| Q16 | `std::thread`s; sensor thread is the **sole owner** of I2C; FRC requests arrive over `mpsc` | Removes both C++ I2C races by construction |
| Q17 | `log` + `EspLogger` (keep C++ log strings), `anyhow` at the edge, plain enums in logic; workspace `logic/` + `firmware/`; NVS keys byte-identical | |
| Q18 | Milestones M0–M5 (below) | M0 fails fast on toolchain; M2 is the point of no return |
| Q19 | Contract check = diff `docs/z2m-baseline.json` vs post-M3 capture | Baseline captured from the live C++ device |
| Q20 | Full rollback rehearsal at M2 (flash `cpp-final`, verify rejoin + self-heal, re-flash Rust) | An untested rollback isn't a rollback |
| Q21 | Soak criteria (below) | |
| Q22 | Root README rewritten for Rust (keep all HW/Z2M/Zigbee knowledge); C++ build gotchas → `legacy-cpp/README.md`; add "Rust gotchas" rows as hit | |
| Q24–Q27 | Ran controlled power-cut experiments before porting the self-heal | The whole self-heal design hinged on the "defect" claim |
| Q28 | Production restored; diagnostic firmware kept on local branch `diag/scd30-persistence` (build machine only) | |
| Q29 | ASC **permanently off** (read at boot, written only if on); every FRC uses Sensirion's procedure (2 s interval for 2 min, FRC, back to 15 s). Applied to C++ now and to the Rust parity spec | Boot FRC always overrides ASC; FRC accuracy per datasheet |
| Q30 | Defect evidence written up for a warranty claim (`docs/scd30-defect-evidence.md`); in Rust the self-heal sits behind a config flag (default on) | A replacement unit makes self-heal optional |

## Facts established

- **Board**: ESP32-C6 (QFN40) rev v0.2, **4 MB flash** (mfr `c4`, dev `6016`) —
  `sdkconfig` claiming 2 MB is wrong. Base MAC `a0:f2:62:45:ce:34`.
- **IEEE** `0xa0f262fffe45ce34` is derived from the eFuse MAC, so it is identical
  under any firmware. Even a forced re-pair keeps Z2M friendly name / HA history.
- **Build/flash host**: `ssh nolik@192.168.10.200` (NixOS). ESP32-C6 on
  `/dev/ttyACM0` (`usb-Espressif_USB_JTAG_serial_debug_unit_A0:F2:62:45:CE:34-if00`).
  **`/dev/ttyUSB0` is the Z2M coordinator (zstack) — never point tools at it.**
- Z2M baseline readable without sudo via retained MQTT topic
  `zigbee2mqtt/bridge/devices` (`nix shell nixpkgs#mosquitto -c mosquitto_sub ...`).
- **C++ latent bug**: `FRC_Trigger_Task` (`main.cpp:489`) issues I2C commands
  concurrently with `SCD30_Task`'s read loop, and `scd30_i2c_bus_recover()` can tear
  down the bus under the other task. Fixed by construction in Q16.
- **SCD30 defect confirmed** (2026-10-10, `docs/scd30-defect-evidence.md`): FRC is lost on
  every real power cut (reads exactly `1.0 ppm` afterwards) with ASC off and with Sensirion's
  2 s FRC procedure; interval, ASC, temperature offset (320) and continuous mode all persist.
  Every FRC is followed by one transient `1.0 ppm` reading.
- SCD30 firmware 3.66. Changing the interval shifts readings ~6 ppm (431 @ 2 s vs 425 @ 15 s).
- C++ app binary is ~600 KB (`0x96110`), a size reference for the Rust build.
- USB port `1-1.2` (behind hub `1-1`) on the build machine is flaky: enumeration errors and
  repeated disconnects after replugging. Each re-enumeration resets `/dev/ttyACM*` to
  `root:dialout 660` and may change the ACM number; always use the `/dev/serial/by-id/` path.
- Opening the ESP's USB-Serial-JTAG port can reset the ESP (not the SCD30).
- Unrelated: `permit_join = true` is permanent in `/etc/nixos/configuration.nix:207`.

## Partition table (Q13)

```
# Name,      Type, SubType, Offset,   Size
nvs,         data, nvs,     0x9000,   0x6000     # unchanged -> self-heal reference kept
phy_init,    data, phy,     0xf000,   0x1000     # unchanged
#            (0x10000-0x110000: old 1 MB factory, unused)
zb_storage,  data, fat,     0x110000, 16K        # unchanged -> pairing kept
zb_fct,      data, fat,     0x114000, 4K         # unchanged
factory,     app,  factory, 0x120000, 2M         # moved + enlarged, ends 0x320000
```

Rollback flashes `cpp-final` with its own `partitions.csv`; app lands at `0x10000`,
`nvs`/`zb_*` untouched.

## Parity spec (Q4) — from `main.cpp` at `8e29f0f`

**Zigbee identity / config** (must match `docs/z2m-baseline.json`)
- Role ZED, `install_code_policy=false`, `ed_timeout=ESP_ZB_ED_AGING_TIMEOUT_64MIN`,
  `keep_alive=3000`, channel mask `ESP_ZB_TRANSCEIVER_ALL_CHANNELS_MASK`.
- Endpoint 1, profile HA, device id `ESP_ZB_HA_TEMPERATURE_SENSOR_DEVICE_ID`, version 0.
- Server clusters: Basic (ZCL version default, power source default, manufacturer
  `Nolik`, model `ESP32C6-SCD30-Zigbee` as ZCL Pascal strings), Identify (default
  time), CO2 (`0.0004` placeholder, min `0.0`, max `0.01`), Temperature (`0`
  placeholder — **never `0x8000`**, min `-4000`, max `8500`), Humidity (`0x8000`
  placeholder, min `0`, max `10000`), On/Off (`0`).
- Value encoding: CO2 = `f32` ppm / 1e6; temperature = `i16` (°C × 100);
  humidity = `u16` (% × 100). Written via `set_attribute_val` under the Zigbee lock,
  never explicit reports (explicit report before bind asserts in the stack).
- Signals: `SKIP_STARTUP` → BDB init; `FIRST_START`/`REBOOT` → steering;
  `STEERING` fail → re-steer.

**SCD30** — I2C addr `0x61`, SDA `GPIO21`, SCL `GPIO7` (never `GPIO0`), 100 kHz,
internal pull-ups, glitch filter 7.
- Boot: read firmware version (log), set interval 15 s, read ASC and write it **off** only if
  on (Q29), start continuous measurement (pressure arg 0); each retried with backoff
  200 ms (<10 tries) / 2 s, full bus recovery every 5 failures.
- **FRC procedure** (all FRCs, Q29): set interval 2 s, wait 120 s, FRC, restore 15 s.
- Self-heal (behind config flag, default on — Q30): load NVS `scd30_cal`/`last_co2` (`i32`);
  if present and plausible (300–10000 ppm) run the FRC procedure with the value clamped
  400–2000, mark trusted on success.
- Loop every 15 s: data-ready → read (CRC-8 0x31/0xFF per word) → set attributes;
  if trusted and plausible, save rounded ppm to NVS every 20th reading; bus recovery
  after 5 consecutive failures.
- Zigbee switch ON (ignored if already in progress): FRC procedure to 425 ppm
  (`SCD30_FRC_REFERENCE_PPM`), save 425 to NVS, write On/Off back to `0`. No ASC toggling.
- Log strings referenced by the README are kept verbatim (e.g.
  `Saved %d ppm as new self-heal reference`, `Self-heal recalibration to %d ppm: OK`).

## Milestones (Q18)

- **M0 Toolchain spike** — flake, workspace, IDF 5.3.3, `esp-zigbee-lib` pulled via
  `extra_components`, bindgen compiles. No join.
- **M1 Sensor** — `logic` crate + host tests, `libscd`, self-heal on hardware with
  serial logs; reads the **existing** NVS reference.
- **M2 Join** — `zb` wrapper; rejoins using existing `zb_storage`, no re-pair.
  Rollback rehearsal (Q20) here.
- **M3 Clusters** — all four clusters report via unmodified converter; contract diff (Q19).
- **M4 FRC switch** — channel to sensor thread, auto-reset.
- **M5 Soak** — 7 days, then delete `legacy-cpp/`.

## Soak criteria (Q21)

- No attribute stuck > 2 min in HA history.
- Zero I2C errors in logs.
- No unplanned reboots (Z2M `last_seen` gaps).
- CO2 daily pattern plausibly matches the previous C++ week.
- Self-heal reference survives ≥ 1 deliberate power cycle.

## Research references (checked 2026-10-10)

- ManfredFr/CO2-Sensor-Zigbee-Rust — Rust CO2 ZED on ESP32-H2, works with Z2M;
  esp-idf-svc 0.51, IDF 5.3.3, bindgen via `extra_components`.
  Traps: no explicit report before bind; `#[no_mangle] extern "C" esp_zb_app_signal_handler`;
  `--undefined=vsnprintf` link arg; no spaces in project path; hand-built
  `esp_zb_cfg_t`/`esp_zb_platform_config_t`. <https://github.com/ManfredFr/CO2-Sensor-Zigbee-Rust>
- canack/huesmith — ESP32-C6/H2, pins `esp-zigbee-lib ==1.6.8` / `esp-zboss-lib ==1.6.4`,
  hand-written bindings; DEVELOPMENT.md documents 4-byte C enums and
  `ESP_ZB_PACKED_STRUCT` layouts. <https://github.com/canack/huesmith>
- zigbee-rs 0.1 — pure Rust, no CO2/OnOff clusters; not viable yet. <https://github.com/zigbee-rs/zigbee-rs>
- Espressif esp-zigbee-sdk — v1.x (ZBOSS) is LTS / bug-fix only; v2 recommended for
  new designs. Unpinned versions resolve to 2.x. <https://github.com/espressif/esp-zigbee-sdk>
- libscd 0.5.1 — sync + async SCD30, FRC/ASC/interval. <https://github.com/SvetlinZarev/libscd>
- Binary size unmeasured; reference projects use 1.5–3 MB app partitions.

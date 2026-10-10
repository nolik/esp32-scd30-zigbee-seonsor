# Rust Firmware Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **User requirement:** apply the `ponytail:ponytail` skill (smallest change that fully solves each step; no extra abstractions, no speculative features) while implementing every task.

**Goal:** Replace the C++ ESP-IDF firmware of the ESP32-C6 + SCD30 Zigbee CO2 sensor with a Rust firmware. It must be a drop-in replacement: same Zigbee identity, same pairing and the same self-heal NVS reference.

**Architecture:** There are two crates. `logic/` is hardware-free and host-tested. It holds the SCD30 service state machine (boot setup, Self-Heal, Reference Recalibration, Stored Reference saving, bus recovery) behind small traits, plus ZCL value encoding. `firmware/` is ESP-IDF `std` Rust (`esp-idf-svc`). It implements those traits with `libscd`, NVS and `std::time`. It also wraps Espressif's closed ZBOSS stack (pinned v1.6.8/1.6.4) through bindgen in one `zb` module, which is the only `unsafe` code. Threads: the Zigbee thread runs the ZBOSS loop. The sensor thread solely owns I2C and receives Reference Recalibration requests over an `mpsc` channel. The two crates are siblings, not one Cargo `[workspace]`: `firmware/.cargo/config.toml` forces the ESP target, while `logic/` must build for the host (`cd logic && cargo test`).

**Tech Stack:** Rust nightly (tier-3 `riscv32imac-esp-espidf`, `build-std`), `esp-idf-svc` 0.51, ESP-IDF v5.3.3, `esp-zigbee-lib ==1.6.8`, `esp-zboss-lib ==1.6.4`, `libscd` 0.5.1, `embuild`, `ldproxy`, `espflash`, Nix flake (nixpkgs-esp-dev + rust-overlay).

**Glossary:** `CONTEXT.md` (Reference Recalibration, Self-Heal, FRC, Stored Reference, Trusted Session, Post-FRC Transient, Measurement Interval, Report Period). Code names follow it.

**Spec:** `docs/rust-migration.md`, the decision record (Q1–Q32, parity spec, partition table, milestones, soak criteria). Supporting files: `docs/z2m-baseline.json` (Zigbee contract) and `docs/scd30-defect-evidence.md` (why the self-heal exists).

## Global Constraints

- Build and flash host: `ssh nolik@192.168.10.200` (NixOS, x86_64), repo at `/home/nolik/esp32-scd30-zigbee-seonsor`.
- ESP32-C6 serial: **always** `/dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_A0:F2:62:45:CE:34-if00`. **Never** touch `/dev/ttyUSB0`; that is the Zigbee2MQTT coordinator.
- Each USB re-enumeration resets the port to `root:dialout 660`. The user must run `sudo chmod 666 /dev/ttyACM*` again, or be added to `dialout`.
- **Never** erase flash (`espflash erase-flash`, `idf.py erase-flash`, `--erase-parts`). It would destroy the Zigbee pairing (`zb_storage`) and the NVS self-heal reference.
- Partition offsets are frozen: `nvs 0x9000/0x6000`, `phy_init 0xf000/0x1000`, `zb_storage 0x110000/16K`, `zb_fct 0x114000/4K`. The Rust `factory` partition is `0x120000`, size `2M`. Flash size is 4 MB.
- ZBOSS SDK v1 pinned exactly: `espressif/esp-zigbee-lib ==1.6.8`, `espressif/esp-zboss-lib ==1.6.4`. ESP-IDF **v5.3.3**.
- Zigbee identity frozen (must match `docs/z2m-baseline.json`):
  - manufacturer `Nolik`, model `ESP32C6-SCD30-Zigbee`
  - EndDevice, endpoint 1, HA profile `0x0104`, device id `0x0302`
  - server clusters Basic, Identify, CO2 `0x040D`, Temperature `0x0402`, Humidity `0x0405`, On/Off `0x0006`
  - `esp32c6_scd30_converter.js` is **not** modified.
- ZED config: `install_code_policy=false`, `ed_timeout=64 min`, `keep_alive=3000`, channel mask `0x07FFF800`.
- NVS self-heal reference: namespace `scd30_cal`, key `last_co2`, type `i32`. These strings are byte-identical to the C++ firmware.
- SCD30:
  - I2C address `0x61`, SDA `GPIO21`, SCL `GPIO7` (never `GPIO0`), 100 kHz, internal pull-ups.
  - Normal measurement interval **15 s**. ASC **off**. Temperature offset **320**. All three are *ensured* at boot: read, and write only if different.
- FRC rules (Q29–Q32):
  - **Self-Heal** at the 15 s interval: wait 120 s, then FRC with the NVS value clamped to 400–2000.
  - **Reference Recalibration** (Zigbee switch): set interval 2 s, wait 120 s, FRC to 425, a 10 ms gap or more, set interval 15 s, save 425, reset the switch.
  - Never run the self-heal at 2 s: it would drift readings about 7 ppm per reboot.
- Leave 10 ms or more between SCD30 commands; back-to-back writes get NACKed.
- Temperature ZCL placeholder is `0`, **never** `0x8000`/`i16::MIN`. That value caused the "temperature stuck at null" bug (signed 16-bit delta overflow in ZBOSS's reporting engine).
- ZCL value encoding: CO2 = `f32` ppm/1e6; temperature = `i16` (°C×100, truncating); humidity = `u16` (%×100, truncating, saturating).
- Plausible CO2 300–10000 ppm. Save the self-heal reference every 20th trusted, plausible reading (~5 min). Bus recovery after 5 consecutive failures.
- Retry backoff for setup commands: 200 ms while there have been fewer than 10 failures, 2 s after that, with a full bus recovery every 5th failure.
- Self-heal is behind Cargo feature `self-heal` (default on, Q30).
- Logging uses `log` with `EspLogger`. Keep these C++ log strings verbatim because the README references them:
  - `Saved {} ppm as new self-heal reference`
  - `Self-heal recalibration to {} ppm: OK`
  - `Self-heal: waiting 2 minutes before reasserting last known CO2 ({} ppm)...`
- All `unsafe` code lives in `firmware/src/zb.rs` and the documented peripheral steal in `firmware/src/sensor_hw.rs`.
- No new dependencies beyond those listed in the Tech Stack line, plus `anyhow`, `log`, `embedded-hal` 1.0.
- **Standing permission (Q11):** flashing the ESP32 firmware over ssh is approved for the whole plan execution, including the `cpp-final` rollback rehearsal. Still **never** erase flash, **never** touch `/dev/ttyUSB0`, and **ask first** before anything that recalibrates the sensor (Task 11) or needs the user physically (unplugging, button presses).
- **Execution (Q12):** native, in this session (superpowers:executing-plans). Pause at the hardware steps of Tasks 8–12 for the user.
- **Branch (Q10):** work on `rust-migration`, created from `main` after PR #1 is merged.
- Do not commit or push without the user asking. Commits use git user `i.novik` / `ihar@getvisibility.com` (repo config). Messages end with `Co-Authored-By: Forcepoint Intelligence Platform <noreply@anthropic.com>`.

## Review Focus

1. **Garbage float from the sensor (NaN/∞ with a valid CRC).** It must not count as plausible, must encode to a safe ZCL value, and must never be saved as the reference. Tests: Task 4 (`clamp_frc_reference`, `is_plausible`, encoding of NaN) and Task 6 (a NaN reading is not saved).
2. **Power cut in the middle of a Reference Recalibration leaves the SCD30 at a 2 s interval.** The next boot must restore 15 s. Test: Task 5 `boot_restores_normal_interval_left_at_frc_interval`.
3. **Zigbee switch flipped during the 2-minute boot self-heal window.** The Reference Recalibration wins, and the self-heal FRC must not also fire. Test: Task 7 `reference_recalibration_supersedes_pending_self_heal`.
4. **Zigbee rejoin (steering signal fires again).** It must not start a second sensor thread or a second I2C owner. Check: Task 9 Step 6 (no `spawn` reachable from the signal handler) and Task 9 Step 9 (log after a forced rejoin).
5. **Attribute writes before the Zigbee stack is initialized** (e.g. self-heal disabled, so the first report comes at second 0). They must be dropped, not crash. Check: Task 10 Step 5 (build with `--no-default-features`, boot log).

---

### Task 1: Freeze the C++ firmware and restructure the repo

**Files:**
- Move: `CMakeLists.txt`, `main/`, `dependencies.lock`, `partitions.csv`, `sdkconfig`, `sdkconfig.defaults`, `sdkconfig.esp32-c6-devkitc-1`, `include/`, `lib/`, `test/` → `legacy-cpp/`
- Delete: `platformio.ini` (Q9)
- Create: `legacy-cpp/README.md`
- Modify: `.gitignore`, `README.md` (short "Repository layout" section at the top)

**Interfaces:**
- Consumes: nothing.
- Produces: tag `cpp-final` (the rollback target used in Task 9). C++ builds from `legacy-cpp/` with `idf.py`.

- [ ] **Step 1: Branch and tag the rollback point.** Wait until PR #1 is merged (ask the user if it isn't yet), then:

```bash
git switch main && git pull
git switch -c rust-migration
git log --oneline -1          # expect the merge commit containing 07765fd
git tag -a cpp-final -m "Last C++ firmware before the Rust migration (rollback target)"
```

- [ ] **Step 2: Move the C++ project.**

```bash
mkdir legacy-cpp
git mv CMakeLists.txt main dependencies.lock partitions.csv sdkconfig sdkconfig.defaults \
       sdkconfig.esp32-c6-devkitc-1 include lib test legacy-cpp/
git rm platformio.ini
```

- [ ] **Step 3: Write `legacy-cpp/README.md`.**

```markdown
# Legacy C++ firmware (rollback only)

Frozen at tag `cpp-final`. Kept buildable until the Rust firmware passes its 7-day soak
(see `docs/rust-migration.md`, Q5/Q21), then deleted.

Build/flash (on the NixOS build host):

    cd legacy-cpp
    nix --experimental-features 'nix-command flakes' develop github:mirrexagon/nixpkgs-esp-dev#esp32c6-idf
    idf.py set-target esp32c6   # first time only
    idf.py -p /dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_A0:F2:62:45:CE:34-if00 flash

Flashing this writes its own partition table (app at 0x10000) but never touches `nvs`,
`zb_storage` or `zb_fct`, so the Zigbee pairing and the self-heal reference survive.

C++-only build gotchas (from the original README): missing `driver` in `REQUIRES`;
`ESP_ZB_*_CONFIG()` / `DEFINE_PSTRING` macros redefined locally in `main.cpp`; narrowing
errors on cluster `measured_value` casts.
```

- [ ] **Step 4: Update `.gitignore`.** Add these lines:

```
# Rust
/target/
/logic/target/
/firmware/target/
/firmware/.embuild/
```

- [ ] **Step 5: Add a layout note at the top of `README.md`,** right under the first heading:

```markdown
> **Repository layout (migration in progress):** `legacy-cpp/` holds the C++ firmware (rollback,
> tag `cpp-final`); `logic/` + `firmware/` hold the Rust firmware; decisions in
> `docs/rust-migration.md`. Build/flash steps below still describe the C++ firmware until Task 12.
```

- [ ] **Step 6: Verify the C++ still builds from its new location** (on the build host, after pulling the branch):

```bash
cd /home/nolik/esp32-scd30-zigbee-seonsor/legacy-cpp
nix --extra-experimental-features 'nix-command flakes' develop github:mirrexagon/nixpkgs-esp-dev#esp32c6-idf -c idf.py build 2>&1 | tail -3
```

Expected: `Project build complete`. If CMake complains about a stale cache, delete `legacy-cpp/build/` (a build artifact) and rerun.

- [ ] **Step 7: Commit** (only when the user asks):

```bash
git add -A && git commit -m "chore: move C++ firmware to legacy-cpp/, drop PlatformIO"
git push origin cpp-final   # only with user approval
```

---

### Task 2: Reproducible Rust + ESP-IDF dev shell

**Files:**
- Create: `flake.nix`, `firmware/rust-toolchain.toml`
- Create (generated): `flake.lock`

**Interfaces:**
- Consumes: nothing.
- Produces: `nix develop` shell with `cargo` (nightly + `rust-src`), `espflash`, `ldproxy`, `IDF_PATH` → ESP-IDF v5.3.3, `LIBCLANG_PATH`, `ESP_IDF_TOOLS_INSTALL_DIR=fromenv`, `MCU=esp32c6`, `mosquitto_sub/pub`, `jq`.

- [ ] **Step 1: Write `firmware/rust-toolchain.toml`.**

```toml
[toolchain]
# riscv32imac-esp-espidf is a tier-3 target: needs nightly + build-std.
# If esp-idf-sys/esp-idf-hal fail to compile on this date, copy the channel pinned by
# https://github.com/ManfredFr/CO2-Sensor-Zigbee-Rust (known-good with esp-idf-svc 0.51).
channel = "nightly-2025-09-01"
components = ["rust-src"]
```

- [ ] **Step 2: Write `flake.nix`.**

```nix
{
  description = "ESP32-C6 SCD30 Zigbee sensor - Rust firmware dev shell";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    esp-dev = {
      url = "github:mirrexagon/nixpkgs-esp-dev";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { nixpkgs, esp-dev, rust-overlay, ... }:
    let
      systems = [ "x86_64-linux" "aarch64-darwin" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f (import nixpkgs {
        inherit system;
        overlays = [ esp-dev.overlays.default (import rust-overlay) ];
        # esptool needs ecdsa (see nixpkgs-esp-dev issue #109)
        config.permittedInsecurePackages = [ "python3.13-ecdsa-0.19.1" ];
      }));
    in {
      devShells = forAllSystems (pkgs:
        let
          # Q12: ESP-IDF 5.3.3 (proven by the Rust reference projects), not the flake default.
          esp-idf = pkgs.esp-idf-riscv.override {
            rev = "v5.3.3";
            sha256 = pkgs.lib.fakeHash; # replace with the hash Nix reports on first use
          };
          rust = pkgs.rust-bin.fromRustupToolchainFile ./firmware/rust-toolchain.toml;
        in {
          default = pkgs.mkShell {
            buildInputs = [ esp-idf rust pkgs.espflash pkgs.ldproxy pkgs.mosquitto pkgs.jq ];
            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
            # Use the IDF + toolchains from this shell instead of embuild downloading its own
            # (downloaded dynamic binaries don't run on NixOS).
            ESP_IDF_TOOLS_INSTALL_DIR = "fromenv";
            MCU = "esp32c6";
          };
        });
    };
}
```

- [ ] **Step 3: Resolve the ESP-IDF hash** (on the build host):

```bash
cd /home/nolik/esp32-scd30-zigbee-seonsor
nix --extra-experimental-features 'nix-command flakes' develop -c true 2>&1 | grep -E "got:|specified:"
```

Expected: a hash mismatch error showing `got: sha256-...`. Put that value in place of `pkgs.lib.fakeHash` and rerun until it succeeds. The first run downloads ESP-IDF with submodules, roughly 1–2 GB.

- [ ] **Step 4: Verify the shell.**

```bash
nix --extra-experimental-features 'nix-command flakes' develop -c sh -c \
  'cargo --version; espflash --version; ldproxy --help >/dev/null && echo ldproxy-ok; echo IDF_PATH=$IDF_PATH; cat $IDF_PATH/version.txt; echo $ESP_IDF_TOOLS_INSTALL_DIR $MCU'
```

Expected: a nightly `cargo`, `espflash 4.x`, `ldproxy-ok`, an `IDF_PATH` in the Nix store, `v5.3.3`, `fromenv esp32c6`.

If `version.txt` shows something else, or the IDF Python dependency check fails (the flake's Python package set targets 5.5.2), **stop and report to the user**. The fallback is the flake default 5.5.2, which revisits Q12.

- [ ] **Step 5: Commit** `flake.nix`, `flake.lock`, `firmware/rust-toolchain.toml` (when the user asks):

```bash
git add flake.nix flake.lock firmware/rust-toolchain.toml
git commit -m "build: pinned Nix dev shell for Rust + ESP-IDF 5.3.3"
```

---

### Task 3: Firmware skeleton with ZBOSS bindings (M0)

**Files:**
- Create: `firmware/Cargo.toml`, `firmware/build.rs`, `firmware/.cargo/config.toml`, `firmware/sdkconfig.defaults`, `firmware/partitions.csv`, `firmware/src/main.rs`, `firmware/src/zb/bindings.h`, `scripts/flash.sh`

**Interfaces:**
- Consumes: Task 2 dev shell.
- Produces: `cargo build --release` in `firmware/` builds an ESP32-C6 image with ZBOSS linked and Rust bindings at `esp_idf_svc::sys::zb::*`. `scripts/flash.sh` flashes bootloader + partition table + app, without erasing anything.

- [ ] **Step 1: `firmware/Cargo.toml`.**

```toml
[package]
name = "firmware"
version = "0.1.0"
edition = "2021"
resolver = "2"

[[bin]]
name = "firmware"
harness = false # no test harness on target

[features]
default = ["self-heal"]
# Q30: re-assert FRC from the NVS reference at boot (needed for this defective SCD30 unit).
self-heal = []

[dependencies]
anyhow = "1"
embedded-hal = "1.0"
esp-idf-svc = "0.51"
libscd = { version = "0.5.1", features = ["sync", "scd30"] }
log = "0.4"
logic = { path = "../logic" }

[build-dependencies]
embuild = "0.33"

[profile.release]
opt-level = "s"
debug = true # symbols stay in the ELF only; not flashed

[profile.dev]
debug = true
opt-level = "z"

[package.metadata.esp-idf-sys]
esp_idf_sdkconfig_defaults = ["sdkconfig.defaults"]

# Q11: Espressif ZBOSS SDK v1, pinned exactly (v2 changes storage format and API).
[[package.metadata.esp-idf-sys.extra_components]]
remote_component = { name = "espressif/esp-zigbee-lib", version = "==1.6.8" }
bindings_header = "src/zb/bindings.h"
bindings_module = "zb"

[[package.metadata.esp-idf-sys.extra_components]]
remote_component = { name = "espressif/esp-zboss-lib", version = "==1.6.4" }
```

- [ ] **Step 2: `firmware/build.rs`.**

```rust
fn main() {
    embuild::espidf::sysenv::output();
}
```

- [ ] **Step 3: `firmware/.cargo/config.toml`.**

```toml
[build]
target = "riscv32imac-esp-espidf"

[target.riscv32imac-esp-espidf]
linker = "ldproxy"
rustflags = ["--cfg", "espidf_time64"]

[unstable]
build-std = ["std", "panic_abort"]

[env]
MCU = "esp32c6"
```

- [ ] **Step 4: `firmware/sdkconfig.defaults`.**

```
# Zigbee requires a 1000 Hz tick
CONFIG_FREERTOS_HZ=1000
CONFIG_ESP_MAIN_TASK_STACK_SIZE=16384
CONFIG_PTHREAD_TASK_STACK_SIZE_DEFAULT=8192

# Real flash size of this board (sdkconfig of the C++ build wrongly said 2MB)
CONFIG_ESPTOOLPY_FLASHSIZE_4MB=y
CONFIG_PARTITION_TABLE_CUSTOM=y
CONFIG_PARTITION_TABLE_CUSTOM_FILENAME="partitions.csv"

# Same ZBOSS library flavour as the C++ firmware (ZCZR lib, used in ED role)
CONFIG_ZB_ENABLED=y
CONFIG_ZB_ZCZR=y
CONFIG_ZB_RADIO_NATIVE=y

CONFIG_LOG_DEFAULT_LEVEL_INFO=y
```

- [ ] **Step 5: `firmware/partitions.csv`** (Q13, keeps the existing offsets):

```
# Name,      Type, SubType, Offset,   Size,     Flags
nvs,         data, nvs,     0x9000,   0x6000,
phy_init,    data, phy,     0xf000,   0x1000,
zb_storage,  data, fat,     0x110000, 16K,
zb_fct,      data, fat,     0x114000, 4K,
factory,     app,  factory, 0x120000, 2M,
```

- [ ] **Step 6: `firmware/src/zb/bindings.h`.**

```c
// Input for bindgen (esp-idf-sys extra_components). Pulls in core, ZCL and ZDO declarations.
#include "esp_zigbee_core.h"
```

- [ ] **Step 7: Minimal `firmware/src/main.rs`.** It only proves that the bindings link.

```rust
use esp_idf_svc::sys::zb as ffi;

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    // Referencing a ZBOSS symbol forces the Zigbee libraries into the link (M0 check only).
    let init: unsafe extern "C" fn(*mut ffi::esp_zb_cfg_t) = ffi::esp_zb_init;
    log::info!("M0 skeleton up, esp_zb_init at {:p}", init as *const ());
    Ok(())
}
```

- [ ] **Step 8: Build.**

```bash
cd /home/nolik/esp32-scd30-zigbee-seonsor/firmware
nix --extra-experimental-features 'nix-command flakes' develop .. -c cargo build --release 2>&1 | tail -20
```

Expected: `Finished release`. The first build compiles ESP-IDF, which takes 10+ minutes.

If it fails, fix only these known issues, each as a separate attempt:
- **Undefined `vsnprintf` at link time:** add `"-C", "link-arg=-Wl,--undefined=vsnprintf"` to `rustflags` (ManfredFr trap).
- **Compile errors inside `esp-idf-sys`/`esp-idf-hal`:** switch the nightly per the comment in `rust-toolchain.toml`.
- **Component manager can't resolve `==1.6.8`:** check the exact version string on <https://components.espressif.com/components/espressif/esp-zigbee-lib>.

Anything else: stop and report the error to the user.

- [ ] **Step 9: Verify the bindings exist.**

```bash
B=$(find target/riscv32imac-esp-espidf/release/build -name bindings.rs -path '*esp-idf-sys*' | head -1)
grep -c "pub fn esp_zb_init" $(dirname $B)/*.rs
grep -o "esp_zb_app_signal_type_t_ESP_ZB_ZDO_SIGNAL_SKIP_STARTUP\|esp_zb_nwk_device_type_t_ESP_ZB_DEVICE_TYPE_ED\|esp_zb_core_action_callback_id_t_ESP_ZB_CORE_SET_ATTR_VALUE_CB_ID" $(dirname $B)/*.rs | sort -u
```

Expected: a non-zero count, and the three constant names. **If the constant names use a different prefix style, note the actual style.** Task 9 uses these exact names.

- [ ] **Step 10: Check image size against the 2 MB partition.**

```bash
espflash save-image --chip esp32c6 target/riscv32imac-esp-espidf/release/firmware /tmp/fw.bin && ls -l /tmp/fw.bin
```

Expected: under 2,097,152 bytes. The C++ app was ~600 KB.

- [ ] **Step 11: `scripts/flash.sh`.** It flashes bootloader + table + app and never erases.

```bash
#!/usr/bin/env bash
# Flash the Rust firmware: bootloader + partition table + app. Never erases flash -
# nvs / zb_storage / zb_fct (pairing + self-heal reference) must survive.
set -euo pipefail
cd "$(dirname "$0")/../firmware"
PORT=/dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_A0:F2:62:45:CE:34-if00
PROFILE=${PROFILE:-release}
ELF=target/riscv32imac-esp-espidf/$PROFILE/firmware
BOOTLOADER=$(find target/riscv32imac-esp-espidf/$PROFILE/build -path '*esp-idf-sys*' -name bootloader.bin | head -1)
[ -n "$BOOTLOADER" ] || { echo "bootloader.bin not found - build first" >&2; exit 1; }
exec espflash flash --port "$PORT" --chip esp32c6 --flash-size 4mb \
  --bootloader "$BOOTLOADER" --partition-table partitions.csv "$ELF" "$@"
```

```bash
chmod +x scripts/flash.sh
```

Do **not** flash yet. The device stays on C++ until Task 8.

- [ ] **Step 12: Commit** (when the user asks):

```bash
git add firmware scripts/flash.sh && git commit -m "feat(firmware): M0 skeleton linking ZBOSS 1.6.8 via bindgen"
```

---

### Task 4: `logic` crate — calibration rules and ZCL encoding

**Files:**
- Create: `logic/Cargo.toml`, `logic/src/lib.rs`, `logic/src/calib.rs`, `logic/src/zcl.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `logic::calib::{MEASUREMENT_INTERVAL_S: u16 = 15, FRC_INTERVAL_S: u16 = 2, TEMPERATURE_OFFSET_CENTI_C: u16 = 320, FRC_REFERENCE_PPM: u16 = 425, SELF_HEAL_WAIT_MS: u64 = 120_000, SAVE_EVERY_READINGS: u32 = 20, BUS_RECOVERY_AFTER_FAILURES: u32 = 5, COMMAND_GAP_MS: u64 = 10, clamp_frc_reference(f32) -> u16, is_plausible(f32) -> bool}`
  - `logic::zcl::{Reading { co2_ppm: f32, temperature_c: f32, humidity_pct: f32 }, Co2Fraction(pub f32), TempCenti(pub i16), HumidityCenti(pub u16), Report { co2, temperature, humidity }, Report::from_reading(&Reading) -> Report, CO2_PLACEHOLDER, TEMP_PLACEHOLDER, HUMIDITY_PLACEHOLDER}`

- [ ] **Step 1: `logic/Cargo.toml` and `logic/src/lib.rs`.**

```toml
[package]
name = "logic"
version = "0.1.0"
edition = "2021"

[dependencies]
log = "0.4"
```

```rust
//! Hardware-free core of the SCD30 Zigbee sensor: calibration rules, ZCL value encoding and
//! the sensor service state machine. Host-tested with `cargo test`; the firmware crate only
//! supplies trait implementations.
pub mod calib;
pub mod zcl;
```

- [ ] **Step 2: Write the failing tests** at the bottom of `logic/src/calib.rs`. Create the file with just the tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_keeps_values_inside_frc_range_rounded() {
        assert_eq!(clamp_frc_reference(436.4), 436);
        assert_eq!(clamp_frc_reference(436.5), 437); // half away from zero, like C lroundf
    }

    #[test]
    fn clamp_limits_to_frc_command_range() {
        assert_eq!(clamp_frc_reference(310.0), 400);
        assert_eq!(clamp_frc_reference(2500.0), 2000);
    }

    #[test]
    fn clamp_maps_nan_to_minimum_instead_of_zero() {
        assert_eq!(clamp_frc_reference(f32::NAN), 400);
    }

    #[test]
    fn plausibility_bounds_are_inclusive_and_reject_garbage() {
        assert!(is_plausible(300.0));
        assert!(is_plausible(10_000.0));
        assert!(!is_plausible(299.9));
        assert!(!is_plausible(1.0)); // the defective unit's uncalibrated floor value
        assert!(!is_plausible(f32::NAN));
        assert!(!is_plausible(f32::INFINITY));
    }
}
```

- [ ] **Step 3: Run the tests and watch them fail.**

```bash
cd logic && cargo test
```

Expected: compile errors, `cannot find function clamp_frc_reference`.

- [ ] **Step 4: Implement `calib.rs`** above the tests:

```rust
//! SCD30 calibration rules shared by boot setup, self-heal and the Reference Recalibration.

/// Normal SCD30 measurement interval; also the firmware's read/report period.
pub const MEASUREMENT_INTERVAL_S: u16 = 15;
/// Interval for the 2 minutes before a manual reference FRC (Sensirion procedure).
pub const FRC_INTERVAL_S: u16 = 2;
/// Sensor-side self-heating correction for 15 s operation (also corrects humidity).
pub const TEMPERATURE_OFFSET_CENTI_C: u16 = 320;
/// Assumed outdoor CO2 for the Reference Recalibration.
pub const FRC_REFERENCE_PPM: u16 = 425;
/// Sensirion: >= 2 minutes of measurements before FRC.
pub const SELF_HEAL_WAIT_MS: u64 = 120_000;
/// Persist a trusted reading as the self-heal reference every N readings (~5 min at 15 s).
pub const SAVE_EVERY_READINGS: u32 = 20;
/// Tear down and recreate the I2C bus after this many consecutive read failures.
pub const BUS_RECOVERY_AFTER_FAILURES: u32 = 5;
/// The SCD30 NACKs commands sent back-to-back.
pub const COMMAND_GAP_MS: u64 = 10;

const FRC_MIN_PPM: u16 = 400;
const FRC_MAX_PPM: u16 = 2000;
const PLAUSIBLE_MIN_PPM: f32 = 300.0;
const PLAUSIBLE_MAX_PPM: f32 = 10_000.0;

/// Clamp a reference to the FRC command's valid range. Written as `!(x >= min)` so NaN
/// lands on the minimum instead of casting to 0 (which the sensor would reject).
pub fn clamp_frc_reference(ppm: f32) -> u16 {
    if !(ppm >= FRC_MIN_PPM as f32) {
        FRC_MIN_PPM
    } else if ppm > FRC_MAX_PPM as f32 {
        FRC_MAX_PPM
    } else {
        ppm.round() as u16
    }
}

/// Readings outside the sensor's accuracy-guaranteed range (incl. the ~1 ppm the defective
/// unit reports when uncalibrated) are never trusted, saved or re-asserted.
pub fn is_plausible(ppm: f32) -> bool {
    (PLAUSIBLE_MIN_PPM..=PLAUSIBLE_MAX_PPM).contains(&ppm)
}
```

- [ ] **Step 5: Run the tests.** `cargo test`. Expected: 4 passed.

- [ ] **Step 6: Write the failing tests** in `logic/src/zcl.rs`, and add `pub mod zcl;` to `lib.rs` if it isn't there yet:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn reading(co2_ppm: f32, temperature_c: f32, humidity_pct: f32) -> Reading {
        Reading { co2_ppm, temperature_c, humidity_pct }
    }

    #[test]
    fn co2_is_reported_as_mol_fraction() {
        let r = Report::from_reading(&reading(425.0, 21.5, 45.5));
        assert!((r.co2.0 - 0.000_425).abs() < 1e-9);
    }

    #[test]
    fn temperature_and_humidity_are_hundredths_truncated() {
        let r = Report::from_reading(&reading(425.0, 21.5, 45.5));
        assert_eq!(r.temperature, TempCenti(2150));
        assert_eq!(r.humidity, HumidityCenti(4550));
        assert_eq!(Report::from_reading(&reading(425.0, -5.25, 0.0)).temperature, TempCenti(-525));
    }

    #[test]
    fn out_of_range_and_nan_values_saturate_instead_of_wrapping() {
        let r = Report::from_reading(&reading(f32::NAN, f32::NAN, -3.0));
        assert_eq!(r.temperature, TempCenti(0));
        assert_eq!(r.humidity, HumidityCenti(0));
        assert_eq!(Report::from_reading(&reading(425.0, 400.0, 900.0)).temperature, TempCenti(i16::MAX));
    }

    #[test]
    fn temperature_placeholder_is_zero_never_the_zcl_invalid_sentinel() {
        // 0x8000 caused the "temperature stuck at null" bug (16-bit delta overflow in ZBOSS).
        assert_eq!(TEMP_PLACEHOLDER, TempCenti(0));
        assert_ne!(TEMP_PLACEHOLDER.0, i16::MIN);
    }
}
```

- [ ] **Step 7: Run the tests and watch them fail.** `cargo test`. Expected: compile errors for `Reading`/`Report`.

- [ ] **Step 8: Implement `zcl.rs`** above the tests:

```rust
//! ZCL attribute value types. Each cluster gets its own type so a wrong cast (the C++
//! firmware's narrowing-conversion trap) is a compile error.

/// One SCD30 measurement in physical units.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Reading {
    pub co2_ppm: f32,
    pub temperature_c: f32,
    pub humidity_pct: f32,
}

/// CO2 Measurement cluster (0x040D) MeasuredValue: single float, mol/mol fraction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Co2Fraction(pub f32);

/// Temperature Measurement cluster (0x0402) MeasuredValue: int16, hundredths of a degree C.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TempCenti(pub i16);

/// Relative Humidity cluster (0x0405) MeasuredValue: uint16, hundredths of a percent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HumidityCenti(pub u16);

/// The three attribute values written after each reading.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Report {
    pub co2: Co2Fraction,
    pub temperature: TempCenti,
    pub humidity: HumidityCenti,
}

impl Report {
    /// Truncating, saturating conversion (Rust `as`), matching the C++ casts without UB.
    pub fn from_reading(r: &Reading) -> Self {
        Self {
            co2: Co2Fraction(r.co2_ppm / 1_000_000.0),
            temperature: TempCenti((r.temperature_c * 100.0) as i16),
            humidity: HumidityCenti((r.humidity_pct * 100.0) as u16),
        }
    }
}

/// Values the clusters start with before the first reading.
pub const CO2_PLACEHOLDER: Co2Fraction = Co2Fraction(0.0004);
/// 0, not 0x8000: see the README "temperature-stuck-at-null" section.
pub const TEMP_PLACEHOLDER: TempCenti = TempCenti(0);
/// 0x8000 = "invalid/not yet measured"; safe for the unsigned humidity attribute.
pub const HUMIDITY_PLACEHOLDER: HumidityCenti = HumidityCenti(0x8000);
```

- [ ] **Step 9: Run the tests.** `cargo test`. Expected: 8 passed.

- [ ] **Step 10: Commit** (when the user asks): `git add logic && git commit -m "feat(logic): calibration rules and typed ZCL encoding"`

---

### Task 5: Sensor service — traits, boot setup, retry

**Files:**
- Create: `logic/src/sensor/mod.rs`, `logic/src/sensor/fakes.rs`, `logic/src/sensor/tests.rs`
- Modify: `logic/src/lib.rs` (add `pub mod sensor;`)

**Interfaces:**
- Consumes: Task 4 `calib`, `zcl::{Reading, Report}`.
- Produces (used by Tasks 6–11):
  - `logic::sensor::Setting { MeasurementInterval, AutoSelfCalibration, TemperatureOffset }`
  - `trait Scd30 { type Error: Debug; fn firmware_version(&mut self) -> Result<(u8, u8), Self::Error>; fn read_setting(&mut self, Setting) -> Result<u16, Self::Error>; fn write_setting(&mut self, Setting, u16) -> Result<(), Self::Error>; fn start_continuous_measurement(&mut self) -> Result<(), Self::Error>; fn data_ready(&mut self) -> Result<bool, Self::Error>; fn read_measurement(&mut self) -> Result<Reading, Self::Error>; fn force_recalibration(&mut self, u16) -> Result<(), Self::Error>; fn recover_bus(&mut self); }`
  - `trait StoredReference { fn load(&mut self) -> Option<i32>; fn save(&mut self, i32) -> bool; }`
  - `trait Reporter { fn report(&mut self, &Report); fn reset_recalibration_switch(&mut self); }`
  - `trait Clock { fn now_ms(&self) -> u64; fn sleep_ms(&mut self, u64); }`
  - `struct Config { pub self_heal_enabled: bool }`
  - `struct SensorService<S, St, R, C>` with `new(S, St, R, C, Config)`, `boot(&mut self)`, `parts(&self) -> (&S, &St, &R, &C)`, `parts_mut(&mut self) -> (&mut S, &mut St, &mut R, &mut C)`

- [ ] **Step 1: Write the fakes** in `logic/src/sensor/fakes.rs`:

```rust
//! Test doubles for the sensor service. Every hardware call is recorded in order.
use std::collections::{HashMap, VecDeque};

use super::{Clock, StoredReference, Reporter, Scd30, Setting};
use crate::zcl::{Reading, Report};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Call {
    FirmwareVersion,
    ReadSetting(Setting),
    WriteSetting(Setting, u16),
    Start,
    DataReady,
    ReadMeasurement,
    Frc(u16),
    RecoverBus,
}

pub struct FakeSensor {
    pub settings: HashMap<Setting, u16>,
    pub read_setting_fails: bool,
    /// The next N `write_setting` calls fail.
    pub fail_writes: u32,
    /// The next N `start_continuous_measurement` calls fail.
    pub fail_start: u32,
    /// Scripted data-ready answers; `Ok(true)` once exhausted.
    pub ready: VecDeque<Result<bool, &'static str>>,
    /// Scripted readings; `default_reading` once exhausted.
    pub readings: VecDeque<Result<Reading, &'static str>>,
    pub default_reading: Reading,
    pub frc_error: Option<&'static str>,
    pub calls: Vec<Call>,
}

impl FakeSensor {
    pub fn new() -> Self {
        let settings = HashMap::from([
            (Setting::MeasurementInterval, 15),
            (Setting::AutoSelfCalibration, 0),
            (Setting::TemperatureOffset, 320),
        ]);
        Self {
            settings,
            read_setting_fails: false,
            fail_writes: 0,
            fail_start: 0,
            ready: VecDeque::new(),
            readings: VecDeque::new(),
            default_reading: Reading { co2_ppm: 500.0, temperature_c: 21.5, humidity_pct: 45.5 },
            frc_error: None,
            calls: Vec::new(),
        }
    }

    pub fn writes(&self) -> Vec<(Setting, u16)> {
        self.calls.iter().filter_map(|c| match c {
            Call::WriteSetting(s, v) => Some((*s, *v)),
            _ => None,
        }).collect()
    }

    pub fn frcs(&self) -> Vec<u16> {
        self.calls.iter().filter_map(|c| match c { Call::Frc(v) => Some(*v), _ => None }).collect()
    }

    pub fn count(&self, call: Call) -> usize {
        self.calls.iter().filter(|c| **c == call).count()
    }
}

impl Scd30 for FakeSensor {
    type Error = &'static str;

    fn firmware_version(&mut self) -> Result<(u8, u8), Self::Error> {
        self.calls.push(Call::FirmwareVersion);
        Ok((3, 66))
    }

    fn read_setting(&mut self, setting: Setting) -> Result<u16, Self::Error> {
        self.calls.push(Call::ReadSetting(setting));
        if self.read_setting_fails {
            return Err("read failed");
        }
        Ok(self.settings[&setting])
    }

    fn write_setting(&mut self, setting: Setting, value: u16) -> Result<(), Self::Error> {
        self.calls.push(Call::WriteSetting(setting, value));
        if self.fail_writes > 0 {
            self.fail_writes -= 1;
            return Err("nack");
        }
        self.settings.insert(setting, value);
        Ok(())
    }

    fn start_continuous_measurement(&mut self) -> Result<(), Self::Error> {
        self.calls.push(Call::Start);
        if self.fail_start > 0 {
            self.fail_start -= 1;
            return Err("nack");
        }
        Ok(())
    }

    fn data_ready(&mut self) -> Result<bool, Self::Error> {
        self.calls.push(Call::DataReady);
        self.ready.pop_front().unwrap_or(Ok(true))
    }

    fn read_measurement(&mut self) -> Result<Reading, Self::Error> {
        self.calls.push(Call::ReadMeasurement);
        self.readings.pop_front().unwrap_or(Ok(self.default_reading))
    }

    fn force_recalibration(&mut self, reference_ppm: u16) -> Result<(), Self::Error> {
        self.calls.push(Call::Frc(reference_ppm));
        self.frc_error.map_or(Ok(()), Err)
    }

    fn recover_bus(&mut self) {
        self.calls.push(Call::RecoverBus);
    }
}

pub struct FakeStore {
    pub value: Option<i32>,
    pub saves: Vec<i32>,
    pub save_ok: bool,
}

impl FakeStore {
    pub fn with(value: Option<i32>) -> Self {
        Self { value, saves: Vec::new(), save_ok: true }
    }
}

impl StoredReference for FakeStore {
    fn load(&mut self) -> Option<i32> {
        self.value
    }
    fn save(&mut self, ppm: i32) -> bool {
        self.saves.push(ppm);
        if self.save_ok {
            self.value = Some(ppm);
        }
        self.save_ok
    }
}

#[derive(Default)]
pub struct FakeReporter {
    pub reports: Vec<Report>,
    pub switch_resets: u32,
}

impl Reporter for FakeReporter {
    fn report(&mut self, report: &Report) {
        self.reports.push(*report);
    }
    fn reset_recalibration_switch(&mut self) {
        self.switch_resets += 1;
    }
}

#[derive(Default)]
pub struct FakeClock {
    pub now: u64,
    pub sleeps: Vec<u64>,
}

impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.now
    }
    fn sleep_ms(&mut self, ms: u64) {
        self.sleeps.push(ms);
        self.now += ms;
    }
}
```

- [ ] **Step 2: Write the failing boot tests** in `logic/src/sensor/tests.rs`:

```rust
use super::fakes::{Call, FakeClock, FakeReporter, FakeSensor, FakeStore};
use super::*;

pub(super) type Svc = SensorService<FakeSensor, FakeStore, FakeReporter, FakeClock>;

pub(super) fn svc(sensor: FakeSensor, stored: Option<i32>) -> Svc {
    SensorService::new(sensor, FakeStore::with(stored), FakeReporter::default(),
                       FakeClock::default(), Config { self_heal_enabled: true })
}

#[test]
fn boot_writes_only_settings_that_differ() {
    let mut sensor = FakeSensor::new();
    sensor.settings.insert(Setting::AutoSelfCalibration, 1);
    sensor.settings.insert(Setting::TemperatureOffset, 460);
    let mut s = svc(sensor, None);
    s.boot();
    let (sensor, ..) = s.parts();
    assert_eq!(sensor.writes(), vec![(Setting::AutoSelfCalibration, 0), (Setting::TemperatureOffset, 320)]);
    assert_eq!(sensor.count(Call::Start), 1);
}

#[test]
fn boot_restores_normal_interval_left_at_frc_interval() {
    // Power cut during a Reference Recalibration leaves the sensor at 2 s (it persists in sensor NVM).
    let mut sensor = FakeSensor::new();
    sensor.settings.insert(Setting::MeasurementInterval, 2);
    let mut s = svc(sensor, None);
    s.boot();
    assert_eq!(s.parts().0.writes(), vec![(Setting::MeasurementInterval, 15)]);
}

#[test]
fn boot_writes_settings_when_they_cannot_be_read() {
    let mut sensor = FakeSensor::new();
    sensor.read_setting_fails = true;
    let mut s = svc(sensor, None);
    s.boot();
    assert_eq!(s.parts().0.writes(), vec![
        (Setting::MeasurementInterval, 15),
        (Setting::AutoSelfCalibration, 0),
        (Setting::TemperatureOffset, 320),
    ]);
}

#[test]
fn setup_commands_retry_with_backoff_and_periodic_bus_recovery() {
    let mut sensor = FakeSensor::new();
    sensor.fail_start = 11;
    let mut s = svc(sensor, None);
    s.boot();
    let (sensor, _, _, clock) = s.parts();
    assert_eq!(sensor.count(Call::Start), 12);
    assert_eq!(sensor.count(Call::RecoverBus), 2); // after failures 5 and 10
    let backoff: Vec<u64> = clock.sleeps.iter().copied().filter(|ms| *ms >= 200).collect();
    assert_eq!(backoff, [vec![200; 9], vec![2000; 2]].concat());
}

#[test]
fn boot_schedules_self_heal_only_for_plausible_reference() {
    for (stored, expect_pending) in [(Some(436), true), (None, false), (Some(150), false), (Some(20_000), false)] {
        let mut s = svc(FakeSensor::new(), stored);
        s.boot();
        assert_eq!(s.self_heal_pending(), expect_pending, "stored={stored:?}");
    }
}

#[test]
fn boot_skips_self_heal_when_disabled_by_config() {
    let mut s = SensorService::new(FakeSensor::new(), FakeStore::with(Some(436)), FakeReporter::default(),
                                   FakeClock::default(), Config { self_heal_enabled: false });
    s.boot();
    assert!(!s.self_heal_pending());
}
```

Add `#[cfg(test)] pub(crate) mod fakes; #[cfg(test)] mod tests;` at the bottom of `mod.rs` (Step 4).

- [ ] **Step 3: Run the tests and watch them fail.** `cargo test`. Expected: compile errors (no `SensorService`).

- [ ] **Step 4: Implement `logic/src/sensor/mod.rs`** (boot part), and add `pub mod sensor;` to `lib.rs`:

```rust
//! SCD30 service: owns the sensor exclusively (one thread), runs boot setup, the boot
//! self-heal FRC, the manual reference FRC and the periodic read/report/save loop.
use crate::calib::*;
use crate::zcl::{Reading, Report};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Setting {
    MeasurementInterval,
    AutoSelfCalibration,
    TemperatureOffset,
}

impl Setting {
    fn name(self) -> &'static str {
        match self {
            Setting::MeasurementInterval => "SCD30 measurement interval",
            Setting::AutoSelfCalibration => "SCD30 ASC",
            Setting::TemperatureOffset => "SCD30 temperature offset",
        }
    }
}

pub trait Scd30 {
    type Error: core::fmt::Debug;
    fn firmware_version(&mut self) -> Result<(u8, u8), Self::Error>;
    fn read_setting(&mut self, setting: Setting) -> Result<u16, Self::Error>;
    fn write_setting(&mut self, setting: Setting, value: u16) -> Result<(), Self::Error>;
    fn start_continuous_measurement(&mut self) -> Result<(), Self::Error>;
    fn data_ready(&mut self) -> Result<bool, Self::Error>;
    fn read_measurement(&mut self) -> Result<Reading, Self::Error>;
    fn force_recalibration(&mut self, reference_ppm: u16) -> Result<(), Self::Error>;
    /// Tear down and recreate the I2C bus (best effort; failures are logged inside).
    fn recover_bus(&mut self);
}

/// The ESP32-side copy of the last trusted CO2 value (survives power loss, unlike the
/// defective sensor's FRC).
pub trait StoredReference {
    fn load(&mut self) -> Option<i32>;
    fn save(&mut self, ppm: i32) -> bool;
}

pub trait Reporter {
    fn report(&mut self, report: &Report);
    /// Return the momentary "Trigger Recalibration" switch to off.
    fn reset_recalibration_switch(&mut self);
}

pub trait Clock {
    fn now_ms(&self) -> u64;
    fn sleep_ms(&mut self, ms: u64);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub self_heal_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelfHeal {
    Inactive,
    Pending { due_ms: u64, reference_ppm: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Recalibration {
    Idle,
    Settling { due_ms: u64 },
}

pub struct SensorService<S, St, R, C> {
    sensor: S,
    store: St,
    reporter: R,
    clock: C,
    config: Config,
    self_heal: SelfHeal,
    recalibration: Recalibration,
    trusted_session: bool,
    trusted_readings: u32,
    consecutive_failures: u32,
    discard_next_reading: bool,
}

impl<S: Scd30, St: StoredReference, R: Reporter, C: Clock> SensorService<S, St, R, C> {
    pub fn new(sensor: S, store: St, reporter: R, clock: C, config: Config) -> Self {
        Self {
            sensor, store, reporter, clock, config,
            self_heal: SelfHeal::Inactive,
            recalibration: Recalibration::Idle,
            trusted_session: false,
            trusted_readings: 0,
            consecutive_failures: 0,
            discard_next_reading: false,
        }
    }

    pub fn parts(&self) -> (&S, &St, &R, &C) {
        (&self.sensor, &self.store, &self.reporter, &self.clock)
    }

    pub fn parts_mut(&mut self) -> (&mut S, &mut St, &mut R, &mut C) {
        (&mut self.sensor, &mut self.store, &mut self.reporter, &mut self.clock)
    }

    pub fn self_heal_pending(&self) -> bool {
        matches!(self.self_heal, SelfHeal::Pending { .. })
    }

    pub fn boot(&mut self) {
        match self.sensor.firmware_version() {
            Ok((major, minor)) => log::info!("SCD30 firmware version: {major}.{minor}"),
            Err(_) => log::warn!("Failed to read SCD30 firmware version"),
        }
        // A power cut during a Reference Recalibration leaves the sensor at 2 s; this restores 15 s.
        self.ensure(Setting::MeasurementInterval, MEASUREMENT_INTERVAL_S);
        // ASC stays off: the boot self-heal FRC always overrides it on this unit.
        self.ensure(Setting::AutoSelfCalibration, 0);
        self.ensure(Setting::TemperatureOffset, TEMPERATURE_OFFSET_CENTI_C);
        self.retry("SCD30 trigger_continuous_measurement", |s| s.start_continuous_measurement());

        if !self.config.self_heal_enabled {
            log::info!("Self-heal disabled by build configuration");
            return;
        }
        match self.store.load() {
            Some(ppm) if is_plausible(ppm as f32) => {
                log::warn!("Self-heal: waiting 2 minutes before reasserting last known CO2 ({ppm} ppm)...");
                self.self_heal = SelfHeal::Pending {
                    due_ms: self.clock.now_ms() + SELF_HEAL_WAIT_MS,
                    reference_ppm: clamp_frc_reference(ppm as f32),
                };
            }
            _ => log::info!("No usable self-heal reference saved yet - skipping auto-recalibration this boot"),
        }
    }

    /// Write a persisted sensor setting only when it differs: every write lands in the
    /// sensor's NVM.
    fn ensure(&mut self, setting: Setting, target: u16) {
        let current = self.sensor.read_setting(setting).ok();
        if current == Some(target) {
            return;
        }
        self.clock.sleep_ms(COMMAND_GAP_MS);
        self.retry(setting.name(), |s| s.write_setting(setting, target));
        log::info!("{} set to {target} (was {current:?})", setting.name());
        self.clock.sleep_ms(COMMAND_GAP_MS);
    }

    /// Retry until success: 200 ms backoff (2 s after 10 failures), full bus recovery every
    /// 5th failure - never abort the device, never spin against a stuck bus.
    fn retry<T>(&mut self, what: &str, mut op: impl FnMut(&mut S) -> Result<T, S::Error>) -> T {
        let mut failures = 0u32;
        loop {
            match op(&mut self.sensor) {
                Ok(value) => return value,
                Err(e) => {
                    failures += 1;
                    log::warn!("{what} failed, retrying... (attempt {failures}): {e:?}");
                    if failures % BUS_RECOVERY_AFTER_FAILURES == 0 {
                        self.sensor.recover_bus();
                    }
                    self.clock.sleep_ms(if failures < 10 { 200 } else { 2000 });
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod fakes;
#[cfg(test)]
mod tests;
```

The fields `reporter`, `recalibration`, `trusted_session`, `trusted_readings`, `consecutive_failures` and `discard_next_reading` trigger dead-code warnings until Tasks 6 and 7. That's acceptable.

- [ ] **Step 5: Run the tests.** `cargo test`. Expected: all Task 4 + 6 new tests pass.

- [ ] **Step 6: Commit** (when the user asks): `git add logic && git commit -m "feat(logic): sensor service boot setup with ensure-settings and retry"`

---

### Task 6: Sensor service — loop, self-heal FRC, saving, bus recovery

**Files:**
- Modify: `logic/src/sensor/mod.rs` (add `tick` and its helpers)
- Modify: `logic/src/sensor/tests.rs` (append tests)

**Interfaces:**
- Consumes: Task 5 types and fakes.
- Produces: `SensorService::tick(&mut self)`, called every `MEASUREMENT_INTERVAL_S` seconds by the firmware loop (Task 8).

- [ ] **Step 1: Append the failing tests** to `tests.rs`:

```rust
fn booted(stored: Option<i32>) -> Svc {
    let mut s = svc(FakeSensor::new(), stored);
    s.boot();
    s.parts_mut().0.calls.clear();
    s
}

fn advance(s: &mut Svc, ms: u64) {
    s.parts_mut().3.now += ms;
}

#[test]
fn no_sensor_traffic_or_reports_while_self_heal_is_pending() {
    let mut s = booted(Some(436));
    advance(&mut s, 60_000);
    s.tick();
    let (sensor, _, reporter, _) = s.parts();
    assert!(sensor.calls.is_empty());
    assert!(reporter.reports.is_empty());
}

#[test]
fn self_heal_fires_frc_with_clamped_reference_and_discards_post_frc_transient() {
    let mut s = booted(Some(2500));
    advance(&mut s, SELF_HEAL_WAIT_MS);
    s.parts_mut().0.readings.push_back(Ok(Reading { co2_ppm: 1.0, temperature_c: 21.0, humidity_pct: 40.0 }));
    s.tick();
    assert_eq!(s.parts().0.frcs(), vec![2000]);
    assert!(s.parts().2.reports.is_empty()); // the ~1 ppm transient is not published
    assert!(!s.self_heal_pending());
    s.tick();
    assert_eq!(s.parts().2.reports.len(), 1);
}

#[test]
fn trusted_session_saves_every_20th_plausible_reading_rounded() {
    let mut s = booted(Some(436));
    advance(&mut s, SELF_HEAL_WAIT_MS);
    s.parts_mut().0.default_reading.co2_ppm = 612.6;
    for _ in 0..41 {
        s.tick(); // the first reading after the FRC is the discarded transient
    }
    assert_eq!(s.parts().1.saves, vec![613, 613]);
}

#[test]
fn implausible_and_nan_readings_are_reported_but_never_counted_or_saved() {
    let mut s = booted(Some(436));
    advance(&mut s, SELF_HEAL_WAIT_MS);
    let sensor = s.parts_mut().0;
    for i in 0..30 {
        let co2 = if i % 2 == 0 { 1.0 } else { f32::NAN };
        sensor.readings.push_back(Ok(Reading { co2_ppm: co2, temperature_c: 21.0, humidity_pct: 40.0 }));
    }
    for _ in 0..30 {
        s.tick();
    }
    assert!(s.parts().1.saves.is_empty());
    assert_eq!(s.parts().2.reports.len(), 29); // first one is the discarded Post-FRC Transient
}

#[test]
fn failed_self_heal_leaves_session_untrusted() {
    let mut sensor = FakeSensor::new();
    sensor.frc_error = Some("nack");
    let mut s = svc(sensor, Some(436));
    s.boot();
    advance(&mut s, SELF_HEAL_WAIT_MS);
    for _ in 0..40 {
        s.tick();
    }
    assert!(s.parts().1.saves.is_empty());
}

#[test]
fn no_reference_means_immediate_reporting_but_no_saving() {
    let mut s = booted(None);
    for _ in 0..25 {
        s.tick();
    }
    assert_eq!(s.parts().2.reports.len(), 25);
    assert!(s.parts().1.saves.is_empty());
}

#[test]
fn data_not_ready_is_neither_a_report_nor_a_failure() {
    let mut s = booted(None);
    for _ in 0..6 {
        s.parts_mut().0.ready.push_back(Ok(false));
    }
    for _ in 0..6 {
        s.tick();
    }
    let (sensor, _, reporter, _) = s.parts();
    assert!(reporter.reports.is_empty());
    assert_eq!(sensor.count(Call::RecoverBus), 0);
}

#[test]
fn five_consecutive_failures_trigger_one_bus_recovery() {
    let mut s = booted(None);
    let sensor = s.parts_mut().0;
    sensor.ready.extend([Err("i2c"), Err("i2c"), Ok(true), Err("i2c"), Err("i2c"), Err("i2c")]);
    sensor.readings.extend([Err("crc"), Err("crc")]);
    for _ in 0..6 {
        s.tick();
    }
    // failures: e, e, read-crc (3), e (4), e (5) -> recovery; 6th tick: read-crc (1)
    assert_eq!(s.parts().0.count(Call::RecoverBus), 1);
}
```

- [ ] **Step 2: Run the tests and watch them fail.** `cargo test`. Expected: compile error, `no method named tick`.

- [ ] **Step 3: Implement**, adding inside the `impl` block in `mod.rs`:

```rust
    /// One loop iteration (every 15 s): due self-heal FRC, then read/report/save.
    pub fn tick(&mut self) {
        if let SelfHeal::Pending { due_ms, reference_ppm } = self.self_heal {
            // Like the C++ firmware: no readings or reports until the self-heal FRC ran -
            // the uncalibrated sensor reports ~1 ppm.
            if self.clock.now_ms() < due_ms {
                return;
            }
            self.self_heal = SelfHeal::Inactive;
            let result = self.sensor.force_recalibration(reference_ppm);
            self.discard_next_reading = true; // Post-FRC Transient
            match result {
                Ok(()) => {
                    log::info!("Self-heal recalibration to {reference_ppm} ppm: OK");
                    self.trusted_session = true;
                }
                Err(e) => log::warn!("Self-heal recalibration FAILED ({e:?})"),
            }
        }
        self.read_and_report();
    }

    fn read_and_report(&mut self) {
        match self.sensor.data_ready() {
            Ok(true) => match self.sensor.read_measurement() {
                Ok(reading) => {
                    self.consecutive_failures = 0;
                    if self.discard_next_reading {
                        // The sensor's first reading after any FRC is ~1 ppm garbage.
                        self.discard_next_reading = false;
                        log::info!("Discarding Post-FRC Transient ({:.1} ppm)", reading.co2_ppm);
                        return;
                    }
                    self.reporter.report(&Report::from_reading(&reading));
                    log::info!(
                        "CO2: {:.1} ppm, Temp: {:.2} C, Humidity: {:.2} %",
                        reading.co2_ppm, reading.temperature_c, reading.humidity_pct
                    );
                    self.maybe_save_reference(reading.co2_ppm);
                }
                Err(_) => self.note_failure("Failed to read SCD30 measurement"),
            },
            Ok(false) => {}
            Err(_) => self.note_failure("Failed to query SCD30 data-ready status"),
        }
    }

    /// Only a Trusted Session may update the Stored Reference - otherwise the ~1 ppm
    /// uncalibrated value would overwrite it.
    fn maybe_save_reference(&mut self, co2_ppm: f32) {
        if !self.trusted_session || !is_plausible(co2_ppm) {
            return;
        }
        self.trusted_readings += 1;
        if self.trusted_readings % SAVE_EVERY_READINGS != 0 {
            return;
        }
        let rounded = co2_ppm.round() as i32;
        if self.store.save(rounded) {
            log::info!("Self-heal reference updated to {rounded} ppm");
        } else {
            log::warn!("Failed to update self-heal reference in NVS");
        }
    }

    fn note_failure(&mut self, what: &str) {
        self.consecutive_failures += 1;
        log::warn!("{what} ({} consecutive)", self.consecutive_failures);
        if self.consecutive_failures >= BUS_RECOVERY_AFTER_FAILURES {
            self.sensor.recover_bus();
            self.consecutive_failures = 0;
        }
    }
```

- [ ] **Step 4: Run the tests.** `cargo test`. Expected: all pass.

- [ ] **Step 5: Commit** (when the user asks): `git add logic && git commit -m "feat(logic): read/report loop, boot self-heal FRC, reference saving, bus recovery"`

---

### Task 7: Sensor service — manual reference FRC

**Files:**
- Modify: `logic/src/sensor/mod.rs`, `logic/src/sensor/tests.rs`

**Interfaces:**
- Consumes: Tasks 5–6.
- Produces: `SensorService::request_reference_recalibration(&mut self) -> bool`. It returns `false` if an FRC is already in progress. The firmware calls it when the Zigbee switch is turned on (Task 11).

- [ ] **Step 1: Append the failing tests:**

```rust
#[test]
fn reference_recalibration_runs_sensirion_procedure_and_restores_normal_interval() {
    let mut s = booted(None);
    assert!(s.request_reference_recalibration());
    assert_eq!(s.parts().0.writes(), vec![(Setting::MeasurementInterval, 2)]);

    advance(&mut s, 60_000);
    s.tick(); // still settling: keeps reporting, no FRC yet
    assert!(s.parts().0.frcs().is_empty());
    assert_eq!(s.parts().2.reports.len(), 1);

    advance(&mut s, 60_000);
    s.tick();
    let (sensor, store, reporter, _) = s.parts();
    assert_eq!(sensor.frcs(), vec![425]);
    assert_eq!(sensor.writes(), vec![(Setting::MeasurementInterval, 2), (Setting::MeasurementInterval, 15)]);
    assert_eq!(store.saves, vec![425]);
    assert_eq!(reporter.switch_resets, 1);
    assert_eq!(reporter.reports.len(), 2);

    s.tick(); // first reading after the FRC: Post-FRC Transient, discarded
    assert_eq!(s.parts().2.reports.len(), 2);
}

#[test]
fn successful_reference_recalibration_starts_trusted_session() {
    // Q4: a fresh board (no Stored Reference) must start saving after a Reference Recalibration.
    let mut s = booted(None);
    s.request_reference_recalibration();
    advance(&mut s, SELF_HEAL_WAIT_MS);
    s.tick(); // finishes the recalibration, saves 425
    for _ in 0..21 {
        s.tick(); // 1 discarded transient + 20 counted readings
    }
    assert_eq!(s.parts().1.saves, vec![425, 500]);
}

#[test]
fn second_request_while_settling_is_ignored() {
    let mut s = booted(None);
    assert!(s.request_reference_recalibration());
    assert!(!s.request_reference_recalibration());
    assert_eq!(s.parts().0.writes().len(), 1);
}

#[test]
fn reference_recalibration_supersedes_pending_self_heal() {
    let mut s = booted(Some(436));
    advance(&mut s, 30_000);
    assert!(s.request_reference_recalibration());
    advance(&mut s, SELF_HEAL_WAIT_MS);
    s.tick();
    assert_eq!(s.parts().0.frcs(), vec![425]); // never 436
    assert!(!s.self_heal_pending());
}

#[test]
fn failed_reference_recalibration_still_restores_interval_and_resets_switch_without_saving() {
    let mut s = booted(None);
    s.parts_mut().0.frc_error = Some("nack");
    s.request_reference_recalibration();
    advance(&mut s, SELF_HEAL_WAIT_MS);
    s.tick();
    let (sensor, store, reporter, _) = s.parts();
    assert_eq!(sensor.writes().last(), Some(&(Setting::MeasurementInterval, 15)));
    assert!(store.saves.is_empty());
    assert_eq!(reporter.switch_resets, 1);
}

#[test]
fn interval_switch_failures_are_retried() {
    let mut s = booted(None);
    s.parts_mut().0.fail_writes = 2;
    s.request_reference_recalibration();
    assert_eq!(s.parts().0.writes(), vec![(Setting::MeasurementInterval, 2); 3]);
}
```

- [ ] **Step 2: Run the tests and watch them fail.** `cargo test`. Expected: `no method named request_reference_recalibration`.

- [ ] **Step 3: Implement.** Add these methods to the `impl` block:

```rust
    /// Zigbee "Trigger Recalibration" switch turned on: reference FRC per Sensirion -
    /// 2 s interval, 2 minutes of measurements, FRC to 425 ppm, back to 15 s.
    pub fn request_reference_recalibration(&mut self) -> bool {
        if self.recalibration != Recalibration::Idle {
            log::warn!("Recalibration already in progress - ignoring trigger");
            return false;
        }
        log::info!("Recalibration triggered via Zigbee");
        if self.self_heal_pending() {
            // The user is exposing the sensor to a real reference; don't also re-assert the
            // stale NVS value two minutes into it.
            log::warn!("Reference Recalibration supersedes the pending Self-Heal");
            self.self_heal = SelfHeal::Inactive;
        }
        log::warn!("Zigbee-triggered FRC: waiting 2 minutes at 2 s interval for stable readings...");
        self.retry("SCD30 set_measurement_interval (FRC)", |s| {
            s.write_setting(Setting::MeasurementInterval, FRC_INTERVAL_S)
        });
        self.recalibration = Recalibration::Settling { due_ms: self.clock.now_ms() + SELF_HEAL_WAIT_MS };
        true
    }

    fn finish_reference_recalibration(&mut self) {
        self.recalibration = Recalibration::Idle;
        let reference = FRC_REFERENCE_PPM;
        let result = self.sensor.force_recalibration(reference);
        self.discard_next_reading = true; // Post-FRC Transient
        self.clock.sleep_ms(COMMAND_GAP_MS);
        self.retry("SCD30 set_measurement_interval (normal)", |s| {
            s.write_setting(Setting::MeasurementInterval, MEASUREMENT_INTERVAL_S)
        });
        match result {
            Ok(()) => {
                log::warn!("Zigbee-triggered FRC to {reference} ppm: OK");
                self.trusted_session = true; // Q4 (deliberate change from the C++ firmware)
                if self.store.save(i32::from(reference)) {
                    log::info!("Saved {reference} ppm as new self-heal reference");
                } else {
                    log::warn!("Failed to save self-heal reference to NVS");
                }
            }
            Err(e) => log::error!("Zigbee-triggered FRC FAILED ({e:?})"),
        }
        self.reporter.reset_recalibration_switch();
    }
```

At the end of `tick()`, after `self.read_and_report();`, add:

```rust
        if let Recalibration::Settling { due_ms } = self.recalibration {
            if self.clock.now_ms() >= due_ms {
                self.finish_reference_recalibration();
            }
        }
```

- [ ] **Step 4: Run the tests.** `cargo test`. Expected: all pass, with no dead-code warnings left.

- [ ] **Step 5: Commit** (when the user asks): `git add logic && git commit -m "feat(logic): manual reference FRC at 2 s with interval restore"`

---

### Task 8: Firmware hardware adapters and on-device sensor bring-up (M1)

**Files:**
- Create: `firmware/src/sensor_hw.rs`, `firmware/src/store.rs`, `firmware/src/clock.rs`
- Modify: `firmware/src/main.rs`

**Interfaces:**
- Consumes: `logic::sensor::*`, `logic::zcl::Reading`, Task 3 build.
- Produces:
  - `sensor_hw::HwScd30::new() -> anyhow::Result<HwScd30>` implementing `logic::sensor::Scd30`
  - `store::NvsStoredReference::new(EspDefaultNvsPartition) -> anyhow::Result<Self>` implementing `StoredReference`
  - `clock::SystemClock::new()` implementing `Clock`
  - `main.rs::run_sensor(nvs: EspDefaultNvsPartition, reporter: impl Reporter + Send + 'static, recalibration_requests: Receiver<()>) -> anyhow::Result<JoinHandle<()>>` (Tasks 9–11 reuse it)

- [ ] **Step 1: `firmware/src/sensor_hw.rs`.**

```rust
//! libscd-backed implementation of the logic crate's `Scd30` trait.
use embedded_hal::i2c::ErrorType;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::gpio::{Gpio21, Gpio7};
use esp_idf_svc::hal::i2c::{I2cConfig, I2cDriver, I2C0};
use esp_idf_svc::hal::units::Hertz;
use libscd::error::Error as ScdError;
use libscd::synchronous::scd30::Scd30 as Driver;
use logic::sensor::{Scd30, Setting};
use logic::zcl::Reading;

type Bus = I2cDriver<'static>;
type BusError = <Bus as ErrorType>::Error;

pub struct HwScd30 {
    driver: Option<Driver<Bus, FreeRtos>>,
}

impl HwScd30 {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self { driver: Some(Self::open()?) })
    }

    fn open() -> anyhow::Result<Driver<Bus, FreeRtos>> {
        // SAFETY: HwScd30 is the only owner of I2C0/GPIO21/GPIO7 - main never takes them from
        // `Peripherals` - and the previous driver is dropped before re-opening, so the steal
        // cannot alias. Stealing is what makes full bus re-creation possible.
        let (i2c, sda, scl) = unsafe { (I2C0::new(), Gpio21::new(), Gpio7::new()) };
        let config = I2cConfig::new()
            .baudrate(Hertz(100_000))
            .sda_enable_pullup(true)
            .scl_enable_pullup(true);
        let bus = I2cDriver::new(i2c, sda, scl, &config)?;
        log::info!("I2C initialized successfully");
        Ok(Driver::new(bus, FreeRtos))
    }

    /// The driver is only missing if a bus recovery failed to re-open; retry that lazily
    /// and report "not allowed" instead of panicking.
    fn drv(&mut self) -> Result<&mut Driver<Bus, FreeRtos>, ScdError<BusError>> {
        if self.driver.is_none() {
            self.driver = Self::open().map_err(|e| log::error!("I2C re-open failed: {e}")).ok();
        }
        self.driver.as_mut().ok_or(ScdError::NotAllowed)
    }
}

impl Scd30 for HwScd30 {
    type Error = ScdError<BusError>;

    fn firmware_version(&mut self) -> Result<(u8, u8), Self::Error> {
        self.drv()?.read_firmware_version()
    }

    fn read_setting(&mut self, setting: Setting) -> Result<u16, Self::Error> {
        let d = self.drv()?;
        match setting {
            Setting::MeasurementInterval => d.get_measurement_interval(),
            Setting::AutoSelfCalibration => d.get_automatic_self_calibration().map(u16::from),
            Setting::TemperatureOffset => d.get_temperature_offset(),
        }
    }

    fn write_setting(&mut self, setting: Setting, value: u16) -> Result<(), Self::Error> {
        let d = self.drv()?;
        match setting {
            Setting::MeasurementInterval => d.set_measurement_interval(value),
            Setting::AutoSelfCalibration => d.enable_automatic_self_calibration(value != 0),
            Setting::TemperatureOffset => d.set_temperature_offset(value),
        }
    }

    fn start_continuous_measurement(&mut self) -> Result<(), Self::Error> {
        // 0 = no ambient pressure compensation (default 1013.25 mbar), as in the C++ firmware
        self.drv()?.start_continuous_measurement(0)
    }

    fn data_ready(&mut self) -> Result<bool, Self::Error> {
        self.drv()?.data_ready()
    }

    fn read_measurement(&mut self) -> Result<Reading, Self::Error> {
        // libscd truncates CO2 to whole ppm (`as u16`); the converter rounds to ppm anyway.
        self.drv()?.read_measurement().map(|m| Reading {
            co2_ppm: f32::from(m.co2),
            temperature_c: m.temperature,
            humidity_pct: m.humidity,
        })
    }

    fn force_recalibration(&mut self, reference_ppm: u16) -> Result<(), Self::Error> {
        self.drv()?.set_forced_recalibration_value(reference_ppm)
    }

    fn recover_bus(&mut self) {
        log::warn!("Attempting full I2C bus/device reset...");
        self.driver = None; // drop releases I2C0 + pins before re-opening
        FreeRtos::delay_ms(100);
        self.driver = Self::open().map_err(|e| log::error!("I2C re-open failed: {e}")).ok();
    }
}
```

- [ ] **Step 2: `firmware/src/store.rs`.**

```rust
//! Self-heal reference in ESP32 NVS. Namespace/key/type are byte-identical to the C++
//! firmware so the existing reference survives the migration (Q2).
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use logic::sensor::StoredReference;

const NAMESPACE: &str = "scd30_cal";
const KEY: &str = "last_co2";

pub struct NvsStoredReference {
    nvs: EspNvs<NvsDefault>,
}

impl NvsStoredReference {
    pub fn new(partition: EspDefaultNvsPartition) -> anyhow::Result<Self> {
        Ok(Self { nvs: EspNvs::new(partition, NAMESPACE, true)? })
    }
}

impl StoredReference for NvsStoredReference {
    fn load(&mut self) -> Option<i32> {
        self.nvs.get_i32(KEY).unwrap_or_else(|e| {
            log::warn!("Reading self-heal reference failed: {e}");
            None
        })
    }

    fn save(&mut self, ppm: i32) -> bool {
        self.nvs.set_i32(KEY, ppm).map_err(|e| log::warn!("NVS write failed: {e}")).is_ok()
    }
}
```

- [ ] **Step 3: `firmware/src/clock.rs`.**

```rust
use std::time::{Duration, Instant};

use logic::sensor::Clock;

pub struct SystemClock {
    start: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self { start: Instant::now() }
    }
}

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    fn sleep_ms(&mut self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}
```

- [ ] **Step 4: `firmware/src/main.rs`** for M1. There's no Zigbee yet, so reports go to the log.

```rust
mod clock;
mod sensor_hw;
mod store;

use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use esp_idf_svc::nvs::EspDefaultNvsPartition;
use logic::calib::MEASUREMENT_INTERVAL_S;
use logic::sensor::{Config, Reporter, SensorService};
use logic::zcl::Report;

/// M1 stand-in until Zigbee reporting exists (Task 10).
struct LogReporter;

impl Reporter for LogReporter {
    fn report(&mut self, _report: &Report) {}
    fn reset_recalibration_switch(&mut self) {
        log::info!("(no Zigbee yet) FRC switch reset");
    }
}

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let nvs = EspDefaultNvsPartition::take()?;
    let (recal_tx, recal_rx) = mpsc::channel::<()>();
    let sensor_thread = run_sensor(nvs, LogReporter, recal_rx)?;
    let _keep_frc_sender_alive = recal_tx; // Task 11 hands it to the Zigbee module
    sensor_thread.join().map_err(|_| anyhow::anyhow!("sensor thread panicked"))?;
    Ok(())
}

/// The sensor thread is the sole owner of the I2C bus (Q16): the driver is created and
/// used only here, and FRC requests arrive as messages - never from another task.
fn run_sensor(
    nvs: EspDefaultNvsPartition,
    reporter: impl Reporter + Send + 'static,
    recalibration_requests: Receiver<()>,
) -> anyhow::Result<std::thread::JoinHandle<()>> {
    let store = store::NvsStoredReference::new(nvs)?;
    Ok(std::thread::Builder::new().name("scd30".into()).stack_size(8 * 1024).spawn(move || {
        let sensor = match sensor_hw::HwScd30::new() {
            Ok(sensor) => sensor,
            Err(e) => {
                log::error!("SCD30 I2C init failed: {e}");
                return;
            }
        };
        let config = Config { self_heal_enabled: cfg!(feature = "self-heal") };
        let mut service = SensorService::new(sensor, store, reporter, clock::SystemClock::new(), config);
        service.boot();
        loop {
            service.tick();
            match recalibration_requests.recv_timeout(Duration::from_secs(u64::from(MEASUREMENT_INTERVAL_S))) {
                Ok(()) => {
                    service.request_reference_recalibration();
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    std::thread::sleep(Duration::from_secs(u64::from(MEASUREMENT_INTERVAL_S)));
                }
            }
        }
    })?)
}
```

- [ ] **Step 5: Build.** `cd firmware && nix ... develop .. -c cargo build --release`. Expected: `Finished`.

If `I2cConfig`, `get_i32`/`set_i32` or the error-type path doesn't compile, look up the exact API in the `esp-idf-svc`/`esp-idf-hal` versions locked in `Cargo.lock` (`cargo doc --open` or docs.rs for that version) and adapt the call. Keep the behaviour unchanged.

- [ ] **Step 6: Flash and verify on the device.** This replaces the running C++ firmware; the device is off Zigbee until Task 9.

```bash
cd /home/nolik/esp32-scd30-zigbee-seonsor && nix --extra-experimental-features 'nix-command flakes' develop -c ./scripts/flash.sh --monitor
```

Expected log within ~3 minutes:
- the boot partition table lists 5 entries with `factory` at `0x00120000`
- `SCD30 firmware version: 3.66`
- no `set to` lines (interval 15, ASC 0 and offset 320 were already correct)
- `Self-heal: waiting 2 minutes before reasserting last known CO2 (N ppm)...`, where N is the value the C++ firmware last saved. **This is the Q2 continuity check**: if it says `No usable self-heal reference`, stop. NVS compatibility is broken.
- `Self-heal recalibration to N ppm: OK`
- `CO2: ...` lines every 15 s, near N

- [ ] **Step 7: Check that the reference persists.** Wait at least 5 minutes, until `Self-heal reference updated to M ppm` appears. Then press the board's RESET button. The next boot must log `reasserting last known CO2 (M ppm)`.

- [ ] **Step 8: Optional between sessions:** reflash `cpp-final` (Task 9 Step 7 commands) to bring the device back on Zigbee.

- [ ] **Step 9: Commit** (when the user asks): `git add firmware && git commit -m "feat(firmware): M1 SCD30 + NVS self-heal on hardware"`

---

### Task 9: Zigbee join with the existing pairing (M2) and rollback rehearsal

**Files:**
- Create: `firmware/src/zb.rs`
- Modify: `firmware/src/main.rs`

**Interfaces:**
- Consumes: Task 3 bindings (`esp_idf_svc::sys::zb`) and the constant-name style noted in Task 3 Step 9.
- Produces:
  - `zb::start(recalibration_requests: Sender<()>) -> anyhow::Result<()>`
  - `zb::ZigbeeReporter`, which implements `logic::sensor::Reporter`. It's completed in Task 10, but declared here so `main` compiles.

- [ ] **Step 1: Check two SDK default values** used for the Basic cluster:

```bash
H=$(find firmware/target -path '*esp-zigbee-lib*' -name 'esp_zigbee_zcl_basic.h' | head -1)
grep -n "ZCL_VERSION_DEFAULT_VALUE\|POWER_SOURCE_DEFAULT_VALUE" "$H"
```

Expected: ZCL version `0x08` and power source `0x01`. The baseline shows "Mains (single phase)", which is `0x01`. If the header differs, use the header's values in Step 2.

- [ ] **Step 2: Write `firmware/src/zb.rs`.**

```rust
//! The only module that touches the ZBOSS C API (all `unsafe` lives here). Builds the frozen
//! Zigbee identity from docs/z2m-baseline.json, runs the stack on its own thread and offers
//! a safe `Reporter` for the sensor thread.
use core::ffi::c_void;
use core::ptr::addr_of_mut;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::OnceLock;

use esp_idf_svc::sys::{esp, zb as ffi};
use logic::sensor::Reporter;
use logic::zcl::{Report, CO2_PLACEHOLDER, HUMIDITY_PLACEHOLDER, TEMP_PLACEHOLDER};

// Numeric values from the ZCL / HA specs (stable across SDK versions).
const ENDPOINT: u8 = 1;
const HA_PROFILE_ID: u16 = 0x0104;
const TEMPERATURE_SENSOR_DEVICE_ID: u16 = 0x0302;
const CLUSTER_ON_OFF: u16 = 0x0006;
const CLUSTER_TEMPERATURE: u16 = 0x0402;
const CLUSTER_HUMIDITY: u16 = 0x0405;
const CLUSTER_CO2: u16 = 0x040D;
const ATTR_MEASURED_VALUE: u16 = 0x0000;
const ATTR_ON_OFF: u16 = 0x0000;
const ATTR_BASIC_MANUFACTURER_NAME: u16 = 0x0004;
const ATTR_BASIC_MODEL_IDENTIFIER: u16 = 0x0005;
const SERVER_ROLE: u8 = 0x01;
const ZCL_TYPE_BOOL: u32 = 0x10;
const ZCL_STATUS_SUCCESS: u32 = 0x00;
const ALL_CHANNELS_MASK: u32 = 0x07FF_F800;
const ED_KEEP_ALIVE_MS: u32 = 3000;
// Task 9 Step 1: values of ESP_ZB_ZCL_BASIC_{ZCL_VERSION,POWER_SOURCE}_DEFAULT_VALUE.
const BASIC_ZCL_VERSION: u8 = 0x08;
const BASIC_POWER_SOURCE: u8 = 0x01;

// ZCL strings are length-prefixed (Pascal style); ZBOSS keeps pointers to them.
static mut MANUFACTURER_NAME: [u8; 6] = *b"\x05Nolik";
static mut MODEL_IDENTIFIER: [u8; 21] = *b"\x14ESP32C6-SCD30-Zigbee";

/// Set once the stack reports SKIP_STARTUP; attribute writes before that are dropped.
static STACK_READY: AtomicBool = AtomicBool::new(false);
/// Where the "Trigger Recalibration" switch sends requests (the sensor thread).
static RECALIBRATION_REQUESTS: OnceLock<Sender<()>> = OnceLock::new();

/// Configure the radio and start the Zigbee stack on its own thread. Call once, after NVS
/// has been initialised.
pub fn start(recalibration_requests: Sender<()>) -> anyhow::Result<()> {
    RECALIBRATION_REQUESTS.set(recalibration_requests).map_err(|_| anyhow::anyhow!("zb::start called twice"))?;
    // SAFETY: plain C struct; all-zero = native radio, no host connection (the defaults the
    // C++ firmware set explicitly).
    let mut platform: ffi::esp_zb_platform_config_t = unsafe { core::mem::zeroed() };
    esp!(unsafe { ffi::esp_zb_platform_config(&mut platform) })?;
    std::thread::Builder::new()
        .name("zigbee".into())
        .stack_size(16 * 1024)
        // SAFETY: run_stack is entered exactly once, on this dedicated thread.
        .spawn(|| unsafe { run_stack() })?;
    Ok(())
}

unsafe fn run_stack() {
    let mut cfg: ffi::esp_zb_cfg_t = core::mem::zeroed();
    cfg.esp_zb_role = ffi::esp_zb_nwk_device_type_t_ESP_ZB_DEVICE_TYPE_ED as _;
    cfg.install_code_policy = false;
    cfg.nwk_cfg.zed_cfg.ed_timeout = ffi::esp_zb_aging_timeout_t_ESP_ZB_ED_AGING_TIMEOUT_64MIN as _;
    cfg.nwk_cfg.zed_cfg.keep_alive = ED_KEEP_ALIVE_MS as _;
    ffi::esp_zb_init(&mut cfg);

    esp!(ffi::esp_zb_device_register(build_endpoint())).expect("esp_zb_device_register");
    ffi::esp_zb_core_action_handler_register(Some(action_handler));
    ffi::esp_zb_set_primary_network_channel_set(ALL_CHANNELS_MASK);

    log::info!("Starting Zigbee Stack...");
    esp!(ffi::esp_zb_start(false)).expect("esp_zb_start");
    ffi::esp_zb_stack_main_loop();
}

unsafe fn build_endpoint() -> *mut ffi::esp_zb_ep_list_t {
    let mut basic_cfg: ffi::esp_zb_basic_cluster_cfg_t = core::mem::zeroed();
    basic_cfg.zcl_version = BASIC_ZCL_VERSION;
    basic_cfg.power_source = BASIC_POWER_SOURCE;
    let basic = ffi::esp_zb_basic_cluster_create(&mut basic_cfg);
    ffi::esp_zb_basic_cluster_add_attr(basic, ATTR_BASIC_MANUFACTURER_NAME, addr_of_mut!(MANUFACTURER_NAME).cast());
    ffi::esp_zb_basic_cluster_add_attr(basic, ATTR_BASIC_MODEL_IDENTIFIER, addr_of_mut!(MODEL_IDENTIFIER).cast());

    let mut identify_cfg: ffi::esp_zb_identify_cluster_cfg_t = core::mem::zeroed(); // identify_time 0

    let mut co2_cfg: ffi::esp_zb_carbon_dioxide_measurement_cluster_cfg_t = core::mem::zeroed();
    co2_cfg.measured_value = CO2_PLACEHOLDER.0;
    co2_cfg.min_measured_value = 0.0;
    co2_cfg.max_measured_value = 0.01; // 10000 ppm

    let mut temp_cfg: ffi::esp_zb_temperature_meas_cluster_cfg_t = core::mem::zeroed();
    temp_cfg.measured_value = TEMP_PLACEHOLDER.0; // 0, never 0x8000 (README Step 6)
    temp_cfg.min_value = -4000;
    temp_cfg.max_value = 8500;

    let mut humidity_cfg: ffi::esp_zb_humidity_meas_cluster_cfg_t = core::mem::zeroed();
    humidity_cfg.measured_value = HUMIDITY_PLACEHOLDER.0;
    humidity_cfg.min_value = 0;
    humidity_cfg.max_value = 10_000;

    let mut on_off_cfg: ffi::esp_zb_on_off_cluster_cfg_t = core::mem::zeroed(); // off

    let list = ffi::esp_zb_zcl_cluster_list_create();
    ffi::esp_zb_cluster_list_add_basic_cluster(list, basic, SERVER_ROLE);
    ffi::esp_zb_cluster_list_add_identify_cluster(list, ffi::esp_zb_identify_cluster_create(&mut identify_cfg), SERVER_ROLE);
    ffi::esp_zb_cluster_list_add_carbon_dioxide_measurement_cluster(
        list, ffi::esp_zb_carbon_dioxide_measurement_cluster_create(&mut co2_cfg), SERVER_ROLE);
    ffi::esp_zb_cluster_list_add_temperature_meas_cluster(list, ffi::esp_zb_temperature_meas_cluster_create(&mut temp_cfg), SERVER_ROLE);
    ffi::esp_zb_cluster_list_add_humidity_meas_cluster(list, ffi::esp_zb_humidity_meas_cluster_create(&mut humidity_cfg), SERVER_ROLE);
    ffi::esp_zb_cluster_list_add_on_off_cluster(list, ffi::esp_zb_on_off_cluster_create(&mut on_off_cfg), SERVER_ROLE);

    let mut ep_cfg: ffi::esp_zb_endpoint_config_t = core::mem::zeroed(); // app_device_version 0
    ep_cfg.endpoint = ENDPOINT;
    ep_cfg.app_profile_id = HA_PROFILE_ID;
    ep_cfg.app_device_id = TEMPERATURE_SENSOR_DEVICE_ID;
    let ep_list = ffi::esp_zb_ep_list_create();
    ffi::esp_zb_ep_list_add_ep(ep_list, list, ep_cfg);
    ep_list
}

fn commission(mode: u32) {
    // SAFETY: called from the stack's own signal handler (Zigbee thread).
    unsafe { ffi::esp_zb_bdb_start_top_level_commissioning(mode as _) };
}

/// Called by ZBOSS on the Zigbee thread. Must never start threads: steering re-fires on every
/// rejoin (the C++ firmware's duplicate-SCD30_Task bug). The sensor thread is started once
/// from `main`.
#[no_mangle]
pub unsafe extern "C" fn esp_zb_app_signal_handler(signal: *mut ffi::esp_zb_app_signal_t) {
    let Some(signal) = signal.as_ref() else { return };
    let sig_type = *signal.p_app_signal;
    let status = signal.esp_err_status;
    match sig_type {
        t if t == ffi::esp_zb_app_signal_type_t_ESP_ZB_ZDO_SIGNAL_SKIP_STARTUP as u32 => {
            log::info!("Zigbee stack initialized");
            STACK_READY.store(true, Ordering::Release);
            commission(ffi::esp_zb_bdb_commissioning_mode_t_ESP_ZB_BDB_MODE_INITIALIZATION as u32);
        }
        t if t == ffi::esp_zb_app_signal_type_t_ESP_ZB_BDB_SIGNAL_DEVICE_FIRST_START as u32
            || t == ffi::esp_zb_app_signal_type_t_ESP_ZB_BDB_SIGNAL_DEVICE_REBOOT as u32 =>
        {
            log::info!("Start network steering");
            commission(ffi::esp_zb_bdb_commissioning_mode_t_ESP_ZB_BDB_MODE_NETWORK_STEERING as u32);
        }
        t if t == ffi::esp_zb_app_signal_type_t_ESP_ZB_BDB_SIGNAL_STEERING as u32 => {
            if status == 0 {
                log::info!("Successfully joined Zigbee network!");
            } else {
                log::warn!("Failed to join network (status: {status}), retrying...");
                commission(ffi::esp_zb_bdb_commissioning_mode_t_ESP_ZB_BDB_MODE_NETWORK_STEERING as u32);
            }
        }
        _ => log::info!("ZDO signal: {sig_type}, status: {status}"),
    }
}

/// Attribute writes from Zigbee2MQTT. Only the On/Off "Trigger Recalibration" switch matters:
/// turning it on sends a request to the sensor thread (which ignores duplicates).
unsafe extern "C" fn action_handler(
    callback_id: ffi::esp_zb_core_action_callback_id_t,
    message: *const c_void,
) -> ffi::esp_err_t {
    if callback_id != ffi::esp_zb_core_action_callback_id_t_ESP_ZB_CORE_SET_ATTR_VALUE_CB_ID {
        log::warn!("Receive Zigbee action(0x{:x}) callback", callback_id as u32);
        return 0;
    }
    let Some(msg) = message.cast::<ffi::esp_zb_zcl_set_attr_value_message_t>().as_ref() else {
        return esp_idf_svc::sys::ESP_FAIL as _;
    };
    if msg.info.status as u32 != ZCL_STATUS_SUCCESS {
        return esp_idf_svc::sys::ESP_ERR_INVALID_ARG as _;
    }
    let is_switch = msg.info.dst_endpoint == ENDPOINT
        && msg.info.cluster == CLUSTER_ON_OFF
        && msg.attribute.id == ATTR_ON_OFF
        && msg.attribute.data.type_ as u32 == ZCL_TYPE_BOOL;
    if is_switch {
        let value = msg.attribute.data.value.cast::<bool>();
        if !value.is_null() && *value {
            if let Some(tx) = RECALIBRATION_REQUESTS.get() {
                let _ = tx.send(());
            }
        }
    }
    0
}

/// Writes ZCL attributes from the sensor thread under the Zigbee lock.
pub struct ZigbeeReporter;

impl ZigbeeReporter {
    fn with_lock(f: impl FnOnce()) {
        if !STACK_READY.load(Ordering::Acquire) {
            log::debug!("Zigbee stack not ready - attribute write dropped");
            return;
        }
        // SAFETY: the ZBOSS API requires holding the stack lock for calls from other tasks.
        unsafe { ffi::esp_zb_lock_acquire(u32::MAX) };
        f();
        unsafe { ffi::esp_zb_lock_release() };
    }
}

/// SAFETY: caller holds the Zigbee lock; `T` matches the attribute's ZCL type.
unsafe fn set_attr<T>(cluster: u16, attr: u16, value: &mut T) {
    let status = ffi::esp_zb_zcl_set_attribute_val(
        ENDPOINT, cluster, SERVER_ROLE, attr, (value as *mut T).cast(), false);
    if status as u32 != ZCL_STATUS_SUCCESS {
        log::warn!("set_attribute_val cluster 0x{cluster:04x} failed: status {}", status as u32);
    }
}

impl Reporter for ZigbeeReporter {
    fn report(&mut self, report: &Report) {
        let (mut co2, mut temp, mut humidity) = (report.co2.0, report.temperature.0, report.humidity.0);
        Self::with_lock(|| unsafe {
            set_attr(CLUSTER_CO2, ATTR_MEASURED_VALUE, &mut co2);
            set_attr(CLUSTER_TEMPERATURE, ATTR_MEASURED_VALUE, &mut temp);
            set_attr(CLUSTER_HUMIDITY, ATTR_MEASURED_VALUE, &mut humidity);
        });
    }

    fn reset_recalibration_switch(&mut self) {
        let mut off = false;
        Self::with_lock(|| unsafe { set_attr(CLUSTER_ON_OFF, ATTR_ON_OFF, &mut off) });
    }
}
```

**Constant names:** use exactly the style found in Task 3 Step 9. If a field's type differs (e.g. `ed_timeout` is `u8`), the `as _` casts already adapt. Fix any remaining type mismatch with an explicit cast; never use `transmute`.

- [ ] **Step 3: Wire it into `main.rs`, joining only for now.** Replace the `main` body with:

```rust
mod zb;
// ...existing mods...

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    // NVS before the Zigbee platform config (avoids "phy_init: NVS has not been initialized").
    let nvs = EspDefaultNvsPartition::take()?;
    let (recal_tx, recal_rx) = mpsc::channel::<()>();
    zb::start(recal_tx)?;
    let sensor_thread = run_sensor(nvs, LogReporter, recal_rx)?; // Task 10 swaps in ZigbeeReporter
    sensor_thread.join().map_err(|_| anyhow::anyhow!("sensor thread panicked"))?;
    Ok(())
}
```

- [ ] **Step 4: Build.** `cargo build --release`. Expected: `Finished`. Fix binding names and types as described in Step 2.

- [ ] **Step 5: Flash and verify the rejoin with the existing `zb_storage`:**

```bash
nix --extra-experimental-features 'nix-command flakes' develop -c ./scripts/flash.sh --monitor
```

Expected log: `Zigbee stack initialized` → `Start network steering` → `Successfully joined Zigbee network!` within ~5 s, **without** enabling permit-join in Z2M. In Z2M, "CO2 sensor" shows a fresh `last_seen` and keeps its friendly name. **This is M2, the point of no return** (see the decision record).

- [ ] **Step 6: Check that the signal handler can't start threads** (Review Focus #4):

```bash
awk '/fn esp_zb_app_signal_handler/,/^}/' firmware/src/zb.rs | grep -c "spawn\|thread::" || true
```

Expected: `0`.

- [ ] **Step 7: Rollback rehearsal (Q20).** Flash the C++ firmware back:

```bash
cd /home/nolik/esp32-scd30-zigbee-seonsor
git worktree add /tmp/cpp-final cpp-final
cd /tmp/cpp-final   # the tag predates Task 1's move: the C++ project is at the root there
nix --extra-experimental-features 'nix-command flakes' develop github:mirrexagon/nixpkgs-esp-dev#esp32c6-idf -c \
  idf.py -p /dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_A0:F2:62:45:CE:34-if00 flash monitor
```

Expected:
- the boot partition table has `factory` at `0x00010000`
- `Successfully joined Zigbee network!`, with no re-pair
- `Self-heal: waiting 2 minutes before reasserting last known CO2 (N ppm)`

- [ ] **Step 8: Flash Rust again:** `cd /home/nolik/esp32-scd30-zigbee-seonsor && nix ... develop -c ./scripts/flash.sh`. Expected: the same rejoin as in Step 5. Then clean up with `git worktree remove /tmp/cpp-final`.

- [ ] **Step 9: Forced-rejoin check** (Review Focus #4). In Z2M, use **Reconfigure** on "CO2 sensor", or restart the coordinator if the user agrees. Then search the serial log: a new `Successfully joined` must **not** be followed by a second `I2C initialized successfully` or `SCD30 firmware version`.

- [ ] **Step 10: Commit** (when the user asks): `git add firmware && git commit -m "feat(firmware): M2 Zigbee join with frozen identity via ZBOSS FFI"`

---

### Task 10: Zigbee reporting and contract verification (M3)

**Files:**
- Modify: `firmware/src/main.rs` (use `zb::ZigbeeReporter`, delete `LogReporter`)
- Create: `scripts/z2m-contract.sh`

**Interfaces:**
- Consumes: `zb::ZigbeeReporter` (Task 9), `run_sensor` (Task 8).
- Produces: a projection of the Z2M device entry comparable to `docs/z2m-baseline.json`.

- [ ] **Step 1: Switch the reporter.** In `main.rs`, delete the `LogReporter` struct and its `impl`, and change the call to:

```rust
    let sensor_thread = run_sensor(nvs, zb::ZigbeeReporter, recal_rx)?;
```

- [ ] **Step 2: Write `scripts/z2m-contract.sh`** (runs on the build host inside `nix develop`):

```bash
#!/usr/bin/env bash
# Print the Z2M contract projection of "CO2 sensor" (same shape as docs/z2m-baseline.json,
# minus capture metadata). Usage: scripts/z2m-contract.sh [--reinterview]
set -euo pipefail
if [ "${1:-}" = "--reinterview" ]; then
  # Without a re-interview Z2M keeps showing endpoints/clusters cached from the C++ firmware.
  mosquitto_pub -h localhost -t zigbee2mqtt/bridge/request/device/interview -m '{"id":"CO2 sensor"}'
  sleep 30
fi
mosquitto_sub -h localhost -t zigbee2mqtt/bridge/devices -C 1 -W 20 | jq -S '
  [.[] | select(.model_id == "ESP32C6-SCD30-Zigbee")][0]
  | {friendly_name, ieee_address, type, manufacturer, model_id, power_source, software_build_id,
     date_code, interview_completed, interview_state, supported,
     endpoints: (.endpoints | map_values({
       bindings: [.bindings[] | {cluster, target: .target.type}],
       clusters,
       configured_reportings: [.configured_reportings[] | {cluster, attribute,
         minimum_report_interval, maximum_report_interval, reportable_change}]}))}'
```

```bash
chmod +x scripts/z2m-contract.sh
```

- [ ] **Step 3: Build, flash, wait for the first reports:**

```bash
nix --extra-experimental-features 'nix-command flakes' develop -c sh -c 'cd firmware && cargo build --release && cd .. && ./scripts/flash.sh'
```

After about 2.5 minutes (the self-heal), check that values arrive:

```bash
mosquitto_sub -h localhost -t "zigbee2mqtt/CO2 sensor" -C 2 -W 120
```

Expected: JSON with `co2` near the self-heal value, a plausible `temperature` (not `0` and not `null`) and `humidity`.

- [ ] **Step 4: Diff the contract (Q19):**

```bash
diff <(jq -S 'del(.captured_at, .source)' docs/z2m-baseline.json) <(./scripts/z2m-contract.sh --reinterview) && echo CONTRACT-OK
```

Expected: `CONTRACT-OK`. Any difference in clusters, identity strings, `power_source` or `type` is a bug in `zb.rs::build_endpoint`. Fix it before continuing. If only the reporting config differs, click **Reconfigure** in Z2M once, then diff again.

- [ ] **Step 5: Writes before the stack is ready** (Review Focus #5). Build without self-heal so the first report comes at second 0:

```bash
nix ... develop -c sh -c 'cd firmware && cargo build --release --no-default-features && cd .. && ./scripts/flash.sh --monitor'
```

Expected: no panic or abort, and the device joins and reports. Then rebuild **with** default features and flash again.

- [ ] **Step 6: Commit** (when the user asks): `git add firmware scripts/z2m-contract.sh && git commit -m "feat(firmware): M3 ZCL reporting + Z2M contract check"`

---

### Task 11: Trigger Recalibration switch (M4)

**Files:**
- Modify: none, if Tasks 8–10 were followed: `zb.rs` already sends on `RECALIBRATION_REQUESTS`, and `run_sensor` already calls `request_reference_recalibration()`. This task is verification only.

**Interfaces:**
- Consumes: `zb::start(recal_tx)`, `run_sensor(.., recal_rx)`, `SensorService::request_reference_recalibration`.
- Produces: an on-device-verified Reference Recalibration.

- [ ] **Step 1: Confirm the wiring.**

```bash
grep -n "RECALIBRATION_REQUESTS.get\|tx.send" firmware/src/zb.rs; grep -n "request_reference_recalibration" firmware/src/main.rs
```

Expected: one hit in each file.

- [ ] **Step 2: On-device test. Ask the user first:** this recalibrates the sensor to 425 ppm. Do it outdoors, or accept an indoor offset until the next real FRC. With the user's OK, run the logger (`flash.sh --monitor`, or the serial logger approach from the decision record) and flip **Trigger Recalibration** in Home Assistant.

Expected log:
- `Recalibration triggered via Zigbee`
- `Zigbee-triggered FRC: waiting 2 minutes at 2 s interval...`
- `CO2:` lines continuing every 15 s
- after 2 minutes: `Zigbee-triggered FRC to 425 ppm: OK` → `Saved 425 ppm as new self-heal reference`
- the switch returns to OFF in Home Assistant on its own
- flipping it again during the 2 minutes logs `Recalibration already in progress - ignoring trigger`

- [ ] **Step 3: Check the interval was restored.** Press RESET. The boot log must **not** contain `SCD30 measurement interval set to 15`, because the interval is already 15.

- [ ] **Step 4: Commit** (if anything changed, when the user asks).

---

### Task 12: Documentation and soak (M5)

**Files:**
- Modify: `README.md` (rewrite build/flash for Rust; keep all hardware/Z2M/Zigbee knowledge)
- Modify: `docs/rust-migration.md` (append a soak log section)
- Delete (only after the soak passes and the user approves): `legacy-cpp/`

**Interfaces:**
- Consumes: everything above.
- Produces: final docs and the soak verdict.

- [ ] **Step 1: README (Q22).**
  - Replace sections 0–3 (env setup, build, partition table, flash) with: `nix develop`, `cd firmware && cargo build --release`, `./scripts/flash.sh [--monitor]`, the "never erase flash" warning and the `/dev/serial/by-id` path.
  - Keep sections 4–6, the troubleshooting table and the constants table. Map the constants to `logic::calib` names.
  - Move the C++-only troubleshooting rows (`driver` REQUIRES, macro redefinitions, narrowing errors) out; they're already in `legacy-cpp/README.md`.
  - Add a "Rust gotchas" subsection to the troubleshooting table with every issue hit in Tasks 3–10. Minimum rows: `--undefined=vsnprintf` (if needed), "no spaces in project path", "`libscd` truncates CO2 to whole ppm (`as u16`), so readings are up to 1 ppm low; accepted, Q7", "ZBOSS asserts on explicit reports before binding (we only use set_attribute_val)", "stale Z2M interview, use `scripts/z2m-contract.sh --reinterview`".

- [ ] **Step 2: Start the 7-day soak (Q5/Q21).** Record the start time in `docs/rust-migration.md` under a new `## Soak log` heading, with a checklist:

```markdown
## Soak log
Start: <date/time>, firmware <commit>
- [ ] No attribute stuck > 2 min (HA history: co2/temperature/humidity)
- [ ] Zero I2C errors (`Failed to`, `bus/device reset` in serial log, if a logger runs)
- [ ] No unplanned reboots (Z2M last_seen gaps; HA shows temperature `0` placeholder only after planned boots)
- [ ] CO2 daily pattern plausible vs previous C++ week
- [ ] Deliberate power cycle mid-week: self-heal re-asserts the NVS reference (log or HA: one 2-min gap, then values near the pre-cut level)
```

- [ ] **Step 3: Mid-week deliberate power cycle.** Ask the user to unplug for 30 s. Check in HA that the readings resume near the level before the cut, after the ~2-minute gap.

- [ ] **Step 4: Soak verdict.** If every box is ticked, ask the user whether to delete `legacy-cpp/`. Rollback stays possible through the `cpp-final` tag. With approval:

```bash
git rm -r legacy-cpp && git commit -m "chore: remove legacy C++ firmware after 7-day Rust soak"
```

If any box fails: rollback (Task 9 Step 7), record the finding in the soak log, and fix before restarting the soak.

- [ ] **Step 5: Commit the docs** (when the user asks): `git add README.md docs && git commit -m "docs: Rust build/flash, gotchas, soak log"`

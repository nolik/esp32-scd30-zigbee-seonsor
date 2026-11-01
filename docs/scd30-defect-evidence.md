# SCD30 defect evidence — FRC calibration lost on every power cycle, ~2 % CO2 sensitivity

Collected 2026-10-10 (FRC loss) and 2026-10-12/13 (sensitivity) on the device in this repo.

**Source of the unit: AliExpress (marketplace).** It may be a clone or a reject rather than a
genuine Sensirion part; over I2C it is indistinguishable (it speaks the full SCD30 protocol and
reports firmware 3.66). Decision (2026-10-10): **no warranty claim**. This document stays as the
record of why the firmware's Self-Heal exists. A replacement sensor from an authorized
distributor (Mouser, Digi-Key, Farnell) is the real fix.

## Unit and setup

| Item | Value |
|---|---|
| Sensor | SCD30 sold as Sensirion (AliExpress, authenticity unverified), I2C address `0x61`, firmware **3.66** (read via `0xD100`) |
| Host | ESP32-C6 (QFN40, rev v0.2), ESP-IDF 5.5.2, `i2c_master` driver |
| Wiring | SDA `GPIO21`, SCL `GPIO7`, ESP internal pull-ups; 100 kHz in production, **50 kHz** in all tests below |
| Supply | from the ESP32-C6 devkit (USB powered) |
| Power cycle method | USB cable unplugged for ~30 s (sensor and host both unpowered) |

## Expected behavior (Sensirion documentation)

Interface Description v1.0 (May 2020), "Set Forced Recalibration value":

> "The FRC method imposes a permanent update of the CO2 calibration curve which
> persists after repowering the sensor. The most recently used reference value is
> retained in volatile memory … After repowering the sensor, the command will
> return the standard reference value of 400 ppm."

<https://sensirion.com/media/documents/D7CEEF4A/6165372F/Sensirion_CO2_Sensors_SCD30_Interface_Description.pdf>

## Observed behavior

After every real power cycle the sensor reports **exactly 1.0 ppm** CO2 (normal
temperature and humidity) indefinitely, until a new FRC is sent. Immediately after
an FRC it reports correct, breath-responsive values again. All other settings stored
in the sensor's non-volatile memory survive the same power cycles.

The diagnostic firmware for tests #1–#3 sent **no** calibration commands at boot.
It only read settings back and logged readings (branch `diag/scd30-persistence`,
local to the build machine).

## Tests

| # | FRC procedure before power cut | ASC | CO2 before cut | CO2 after cut |
|---|---|---|---|---|
| 1 | FRC 436 ppm at 15 s interval (earlier self-heal) | on | 435–437 ppm (13:44–13:52) | **1.0 ppm** (13:54 → stable from 14:01, 14 readings) |
| 2 | FRC 425 ppm at 15 s interval, after a 2 min wait | **off** | 424–425 ppm (14:10–14:13) | **1.0 ppm** (14:14:49 →) |
| 3 | Interval set to **2 s** (readback = 2), 3 min settling, FRC 425 ppm, 60 s hold, interval back to 15 s | off | 417–425 ppm (14:31–14:34) | **1.0 ppm** (14:35:15 →) |

Times are local (CEST), 2026-10-10. Test #3 follows Sensirion's stated precondition
("measurement rate of 2s for at least two minutes before applying the FRC").

## Settings read back after the power cuts (persistence works for everything except FRC)

| Setting (command) | Before cut | After cut |
|---|---|---|
| Measurement interval (`0x4600`) | 15 | 15 |
| ASC (`0x5306`) | 0, written ~7 min before the cut | 0 |
| Temperature offset (`0x5403`) | 320 | 320 |
| Altitude (`0x5102`) | 0 | 0 |
| Continuous measurement | running | running (resumed by itself, no start command sent) |
| FRC reference readback (`0x5204`) | 436 / 425 | 400 (expected per datasheet) |
| **CO2 reading** | **≈ 425 ppm** | **1.0 ppm** |

## Ruled out

- **Firmware writing calibration at boot:** the diagnostic firmware sent nothing calibration-related.
- **ASC overriding FRC:** test #2 had ASC off, written and confirmed persisted.
- **FRC procedure:** test #3 used Sensirion's 2 s / ≥ 2 min precondition.
- **NVM failure in general:** the interval, ASC, temperature offset and continuous mode all persisted.
- **Warm-up:** the 1.0 ppm reading stays for 5+ minutes. The datasheet warm-up is < 10 s.
- **Dead NDIR source / no signal:** every FRC immediately restores readings that respond to breath.
  But see the next section: the response is only a few percent of the real change.

## Second defect: CO2 sensitivity ~2 % of the real change (night 2026-10-12/13)

Collected from the Zigbee2MQTT journal (Rust firmware, router role, 15 s interval, Self-Heal
active). The user slept alone in the closed bedroom: 13 m² (ceiling ~2.6 m assumed → ~34 m³).

| Time | CO2 (ppm) | RH (%) | T (°C) | Absolute humidity (g/m³) |
|---|---|---|---|---|
| 10-12 22:10 | 422 | 53.4 | 19.4 | 8.9 |
| 10-13 01:24 | 429 | 62.6 | 20.1 | 10.9 |
| 10-13 05:23 | 438 | 63.5 | 21.1 | 11.7 |
| 10-13 08:23 | 441 (peak) | 63.5 | 21.3 | 11.8 |
| 10-13 10:06 (room opened) | 423 | 58–60 | 20.7 | 11.5 |

CO2 is reported on a change of ≥ 10 ppm or hourly. Overnight the CO2 reports came only on the
hourly timer, so the reading never moved 10 ppm between them.

- **Measured:** CO2 +19 ppm over ~10 h. Water vapour +2.9 g/m³ (≈ 3900 ppmv), levelling off
  around 05:00. That proves an occupied, closed room.
- **Air exchange from the vapour:** a sleeper gives off ~30–40 g/h of water, so +2.9 g/m³ at
  steady state means ~12 m³/h (≈ 0.35 air changes per hour). Moisture buffering by walls and
  bedding makes the real exchange lower.
- **Expected CO2:** a sleeper gives off ~13–15 L/h of CO2. Expected rise ≈ 15 L/h ÷ 12 m³/h ≈
  **+1000–1300 ppm**, and at least +440 ppm even at 1 air change per hour.
- **Ratio:** the sensor showed **~1.5–2 %** of the expected change (≤ 4 % in the leakiest case).

### Breath test (2026-10-14)

Two rounds of three slow exhales from ~5 cm onto the sensor, 15 s interval.

| Time | CO2 (ppm) | RH (%) | T (°C) |
|---|---|---|---|
| 18:14:19 (baseline) | 416 | 60 | 22.5 |
| 18:16:05 (peak, round 1) | 539 | 80–100 | 26.9 |
| 18:17:05 | 418 | 57 | 25.6 |
| 18:20:35 (peak, round 2) | 601 | 100 | 26.2 |

- **The breath reached the module:** the SCD30's own humidity and temperature sensor saturated
  at 100 % RH and rose ~4 °C.
- **CO2 rose only +120 to +185 ppm.** Exhaled air is ~40,000–50,000 ppm, and a healthy SCD30 at
  this distance typically shows 2000–10,000 ppm. That is ~2–5 % of the exposure, the same order as
  the night.
- **Response time is normal:** the reading rose within one 15 s reading and was back at baseline
  in ~1 min. Air reaches the optical cell, so a blocked inlet (dust) is ruled out.

### Known-dose pot test (2026-11-01)

A known amount of CO2 released into a closed ~7 L pot (glass lid). 10 ml of 6 % vinegar topped up
to 400 ml water; 40 ml of that mix (1.0 mmol acetic acid) poured over excess baking soda in a
glass inside the pot. The board sat raised next to it, open to the pot air. 1.0 mmol → ~24 ml
CO2 → **+3,400 ppm expected** (+2,400–3,400 allowing for CO2 left dissolved). Readings were taken
over Zigbee by reading `msCO2.measuredValue` once a minute.

| Time | CO2 (ppm) | RH (%) | T (°C) |
|---|---|---|---|
| before the lid | 425 | — | — |
| 14:11 (lid on ~60 min) | 458 | 74.2 | 22.3 |
| 14:19 | 460 | 74.2 | 23.3 |
| 14:27 (lid on ~75 min) | 460 | 75.3 | 23.3 |
| 14:41 (lid on ~90 min) | 460 | 76.3 | 23.3 |

- **Plateau, not a slow rise:** 458–460 ppm for the last 30 minutes, so the release was complete.
- **Sealed:** humidity held at 74–76 %.
- **Measured +35 ppm against +2,400–3,400 expected: ~1–1.5 % sensitivity**, matching the night and
  breath tests. Not yet checked: that the soda reacted fully (a splash of undiluted vinegar into
  the glass should still fizz). Even at half the dose it would be ~3 %.

### Firmware ruled out

The firmware path was checked and holds no scale error:

| Step | Conversion |
|---|---|
| SCD30 output | big-endian float in ppm |
| libscd 0.5.1 | `f32::from_be_bytes` over the two words, CRC bytes skipped |
| Firmware | ÷ 10⁶ to the ZCL mol/mol fraction |
| Z2M converter | × 10⁶ back to ppm |

A scale bug would also scale the ~425 ppm baseline, which matches the FRC reference exactly. The
old C++ firmware, with its own I2C decoding, showed the same flat 418–427 ppm indoors. The SCD30
has no user-settable gain: FRC and ASC adjust the offset only. So each Self-Heal pins a plausible
~425 ppm onto a sensor that barely responds to CO2. This fits a weak NDIR path (IR source,
detector or optics), and the 1.0 ppm post-power-cut reading is consistent with it. **The CO2
values from this unit are not usable**; temperature and humidity look plausible.

## Workaround in firmware

The firmware stores the last trusted CO2 reading in ESP32 NVS every ~5 min. At boot
it re-applies FRC with that value after 2 min of settling at the normal 15 s interval - the
interval the value was measured at (a 2 s Self-Heal would shift readings ~7 ppm per reboot,
see `docs/adr/0003`). The accuracy is limited by how much the room's CO2 changed during the outage.

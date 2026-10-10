# SCD30 defect evidence — FRC calibration lost on every power cycle

Collected 2026-10-10 on the device in this repo.

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
- **Dead NDIR source / no signal:** every FRC immediately restores correct readings that respond to breath.

## Workaround in firmware

The firmware stores the last trusted CO2 reading in ESP32 NVS every ~5 min. At boot
it re-applies FRC with that value, at a 2 s interval after 2 min of settling. The
accuracy is limited by how much the room's CO2 changed during the outage.

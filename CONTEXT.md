# SCD30 Zigbee CO2 Sensor

A DIY Zigbee device (a router) that measures CO2, temperature and humidity with a Sensirion SCD30.
Its SCD30 unit forgets its calibration on every power loss, and the firmware compensates for that.

## Language

### Calibration

**FRC**:
The SCD30's forced-recalibration command: it tells the sensor "what you measure right now is N ppm".
It is the low-level operation behind both Reference Recalibration and Self-Heal.
_Avoid_: using "FRC" for either of those two procedures

**Reference Recalibration**:
A user-triggered calibration against air of known CO2 concentration (fresh outdoor air,
assumed 425 ppm), started with the "Trigger Recalibration" switch.
_Avoid_: manual FRC, calibration, seeding

**Self-Heal**:
The automatic re-application of the Stored Reference at every boot, needed because this unit loses
its calibration on power loss.
_Avoid_: self-heal FRC, auto-recalibration, reassert

**Stored Reference**:
The CO2 value kept in the device's own non-volatile memory for Self-Heal to re-apply.
_Avoid_: last known CO2, self-heal reference, NVS reference, FRC reference value

**Trusted Session**:
A boot in which a Self-Heal or Reference Recalibration succeeded. Only a Trusted Session may
update the Stored Reference.
_Avoid_: calibration trusted, trusted calibration

**Post-FRC Transient**:
The single meaningless (~1 ppm) reading the sensor produces right after any FRC. It is discarded.
_Avoid_: glitch, spike

### Timing

**Measurement Interval**:
How often the SCD30 itself takes a measurement. It is normally slow and is shortened only during a
Reference Recalibration.
_Avoid_: polling interval, sampling rate, pulling interval

**Report Period**:
How often the firmware reads the latest measurement and publishes it over Zigbee.
_Avoid_: polling interval, measurement interval

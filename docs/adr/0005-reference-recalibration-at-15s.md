# Reference Recalibration runs at the 15 s Measurement Interval

Supersedes [ADR 0003](0003-self-heal-at-15s-reference-recalibration-at-2s.md).

On this SCD30 unit, changing the Measurement Interval shifts CO2 readings by about 7 ppm (measured
2026-10-10). The module warms itself more at 2 s, and the FRC correction is a fixed number. A
Reference Recalibration at 2 s therefore left the 15 s readings at ~418 ppm in the air it had just
called 425. Sensirion's low-power application note says the same: changing the interval reduces CO2
accuracy, so recalibrate at the interval you run.

ADR 0003 accepted that 7 ppm as a constant offset inside the ±30 ppm spec. The Broken sensor
adjustments (README) scale every raw ppm by 50, so it became ~350 ppm: fresh air read 75 ppm and
was clamped to the 400 ppm floor (2026-11-01, two recalibrations at an open window).

Both FRCs now run at 15 s: Reference Recalibration waits 5 minutes at 15 s (20 readings) and applies
FRC = 425 without changing the interval. Self-Heal already worked this way. The accepted cost: the
procedure departs from Sensirion's 2 s FRC precondition. Boot still writes 15 s if the sensor is
at another interval (a power cut during an older 2 s recalibration left it there).

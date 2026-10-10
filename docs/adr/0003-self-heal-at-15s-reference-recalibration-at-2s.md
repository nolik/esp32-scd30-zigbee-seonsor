# Self-Heal runs at the 15 s Measurement Interval, Reference Recalibration at 2 s

On this SCD30 unit, changing the Measurement Interval shifts CO2 readings by about 7 ppm (measured
2026-10-10, 10 switches between 2 s, 5 s and 15 s). Normal operation uses 15 s. A Reference
Recalibration follows Sensirion's procedure (2 s for 2 minutes, FRC, back to 15 s). Self-Heal
re-applies a Stored Reference that was measured at 15 s, so it must run its FRC at 15 s. Running it
at 2 s would make the readings about 7 ppm lower after every boot, and that error would build up
through the Stored Reference. The accepted cost: after a Reference Recalibration, the 15 s readings
sit about 7 ppm below the reference, and that offset stays constant. Do not "unify" the two
intervals without removing that drift first.

//! Broken sensor adjustments: a stopgap for this SCD30 unit, which shows only ~1-2 % of a real
//! CO2 change (docs/scd30-defect-evidence.md, pot test 2026-11-01). Only the reported CO2 is
//! scaled; the Stored Reference and every FRC keep using the raw value. After the sensor is
//! replaced, drop the `broken-sensor-adjustment` firmware feature (README "Broken sensor
//! adjustments").
use crate::calib::FRC_REFERENCE_PPM;

/// Broken sensor adjustments value: CO2 gain. ×50 fits the overnight test; the pot test
/// suggested ×70-100.
pub const BROKEN_SENSOR_CO2_GAIN: f32 = 50.0;
/// Broken sensor adjustments value: raw reading that maps to itself (the fresh-air FRC point).
pub const BROKEN_SENSOR_BASELINE_PPM: f32 = FRC_REFERENCE_PPM as f32;
/// Broken sensor adjustments value: lowest reported CO2 (raw dips below baseline ×50 are noise).
pub const BROKEN_SENSOR_FLOOR_PPM: f32 = 400.0;
/// Broken sensor adjustments value: raw readings averaged before scaling (4 × 15 s = 1 min),
/// so the ±1 ppm raw jitter doesn't become ±50 ppm.
pub const BROKEN_SENSOR_AVERAGE_READINGS: usize = 4;

/// Moving average of the raw CO2, scaled around the baseline.
#[derive(Debug, Clone, Default)]
pub struct BrokenSensorCo2 {
    recent: [f32; BROKEN_SENSOR_AVERAGE_READINGS],
    len: usize,
    next: usize,
}

impl BrokenSensorCo2 {
    pub fn adjust(&mut self, raw_ppm: f32) -> f32 {
        self.recent[self.next] = raw_ppm;
        self.next = (self.next + 1) % BROKEN_SENSOR_AVERAGE_READINGS;
        self.len = (self.len + 1).min(BROKEN_SENSOR_AVERAGE_READINGS);
        let mean = self.recent[..self.len].iter().sum::<f32>() / self.len as f32;
        // f32::max drops a NaN operand, so a garbage reading reports the floor.
        (BROKEN_SENSOR_BASELINE_PPM + BROKEN_SENSOR_CO2_GAIN * (mean - BROKEN_SENSOR_BASELINE_PPM))
            .max(BROKEN_SENSOR_FLOOR_PPM)
    }

    /// Forget readings taken before an FRC: they belong to the old calibration.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(adj: &mut BrokenSensorCo2, raws: &[f32]) -> Vec<f32> {
        raws.iter().map(|r| adj.adjust(*r)).collect()
    }

    #[test]
    fn baseline_maps_to_itself_and_each_raw_ppm_above_counts_fifty() {
        assert_eq!(feed(&mut BrokenSensorCo2::default(), &[425.0]), vec![425.0]);
        // Pot test plateau: raw 460 -> 425 + 50 * 35.
        assert_eq!(feed(&mut BrokenSensorCo2::default(), &[460.0]), vec![2175.0]);
    }

    #[test]
    fn averages_the_last_four_readings() {
        let mut adj = BrokenSensorCo2::default();
        // Means: 425, 427, 428, 428.75 (window full), then 425 drops out: 430.75.
        let out = feed(&mut adj, &[425.0, 429.0, 430.0, 431.0, 433.0]);
        assert_eq!(out, vec![425.0, 525.0, 575.0, 612.5, 712.5]);
    }

    #[test]
    fn below_baseline_and_nan_report_the_floor() {
        assert_eq!(feed(&mut BrokenSensorCo2::default(), &[416.0]), vec![400.0]);
        assert_eq!(feed(&mut BrokenSensorCo2::default(), &[f32::NAN]), vec![400.0]);
    }

    #[test]
    fn reset_drops_old_readings() {
        let mut adj = BrokenSensorCo2::default();
        feed(&mut adj, &[460.0, 460.0]);
        adj.reset();
        assert_eq!(adj.adjust(425.0), 425.0);
    }
}

//! SCD30 calibration rules shared by boot setup, Self-Heal and Reference Recalibration.

/// Normal SCD30 Measurement Interval; also the firmware's Report Period.
pub const MEASUREMENT_INTERVAL_S: u16 = 15;
/// Measurement Interval for the 2 minutes before a Reference Recalibration (Sensirion procedure).
pub const FRC_INTERVAL_S: u16 = 2;
/// Sensor-side self-heating correction for 15 s operation (also corrects humidity).
pub const TEMPERATURE_OFFSET_CENTI_C: u16 = 320;
/// Assumed outdoor CO2 for a Reference Recalibration.
pub const FRC_REFERENCE_PPM: u16 = 425;
/// Sensirion: >= 2 minutes of measurements before FRC.
pub const SELF_HEAL_WAIT_MS: u64 = 120_000;
/// Persist a trusted reading as the Stored Reference every N readings (~5 min at 15 s).
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

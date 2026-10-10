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

/// Value ranges declared for the clusters (zb.rs uses the same constants).
pub const CO2_MAX_FRACTION: f32 = 0.01; // 10000 ppm
pub const TEMP_MIN_CENTI: i16 = -4000;
pub const TEMP_MAX_CENTI: i16 = 8500;
pub const HUMIDITY_MAX_CENTI: u16 = 10_000;

impl Report {
    /// Truncating conversion clamped to the declared ranges, so a garbage float can never
    /// produce the 0x8000 temperature sentinel or an out-of-range value.
    pub fn from_reading(r: &Reading) -> Self {
        let centi = |v: f32| (v * 100.0) as i32; // saturating; NaN -> 0
        Self {
            co2: Co2Fraction((r.co2_ppm / 1_000_000.0).clamp(0.0, CO2_MAX_FRACTION)),
            temperature: TempCenti(centi(r.temperature_c).clamp(TEMP_MIN_CENTI.into(), TEMP_MAX_CENTI.into()) as i16),
            humidity: HumidityCenti(centi(r.humidity_pct).clamp(0, HUMIDITY_MAX_CENTI.into()) as u16),
        }
    }
}

/// Values the clusters start with before the first reading.
pub const CO2_PLACEHOLDER: Co2Fraction = Co2Fraction(0.0004);
/// 0, not 0x8000: see the README "temperature-stuck-at-null" section.
pub const TEMP_PLACEHOLDER: TempCenti = TempCenti(0);
/// 0x8000 = "invalid/not yet measured"; safe for the unsigned humidity attribute.
pub const HUMIDITY_PLACEHOLDER: HumidityCenti = HumidityCenti(0x8000);

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
    }

    #[test]
    fn garbage_is_clamped_to_the_declared_cluster_ranges() {
        // A CRC-valid garbage float must never produce the banned 0x8000 temperature sentinel.
        let low = Report::from_reading(&reading(f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY));
        assert_eq!(low.temperature, TempCenti(-4000));
        assert_eq!(low.humidity, HumidityCenti(10_000));
        assert_eq!(low.co2, Co2Fraction(0.01));
        let high = Report::from_reading(&reading(-5.0, 400.0, -3.0));
        assert_eq!(high.temperature, TempCenti(8500));
        assert_eq!(high.humidity, HumidityCenti(0));
        assert_eq!(high.co2, Co2Fraction(0.0));
    }

    #[test]
    fn temperature_placeholder_is_zero_never_the_zcl_invalid_sentinel() {
        // 0x8000 caused the "temperature stuck at null" bug (16-bit delta overflow in ZBOSS).
        assert_eq!(TEMP_PLACEHOLDER, TempCenti(0));
        assert_ne!(TEMP_PLACEHOLDER.0, i16::MIN);
    }
}

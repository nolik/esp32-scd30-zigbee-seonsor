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

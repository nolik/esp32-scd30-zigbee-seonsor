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

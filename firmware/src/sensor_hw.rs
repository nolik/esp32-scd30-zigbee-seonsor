//! libscd-backed implementation of the logic crate's `Scd30` trait.
use std::time::Duration;

use embedded_hal::i2c::ErrorType;
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::hal::gpio::{Gpio21, Gpio7};
use esp_idf_svc::hal::i2c::{I2cConfig, I2cDriver, I2C0};
use esp_idf_svc::hal::units::Hertz;
use libscd::error::Error as ScdError;
use libscd::synchronous::scd30::Scd30 as Driver;
use logic::sensor::{Scd30, Setting};
use logic::zcl::Reading;

type Bus = I2cDriver<'static>;
type BusError = <Bus as ErrorType>::Error;

pub struct HwScd30 {
    driver: Option<Driver<Bus, FreeRtos>>,
}

impl HwScd30 {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self { driver: Some(Self::open()?) })
    }

    fn open() -> anyhow::Result<Driver<Bus, FreeRtos>> {
        // SAFETY: HwScd30 is the only owner of I2C0/GPIO21/GPIO7 - main never takes them from
        // `Peripherals` - and the previous driver is dropped before re-opening, so the steal
        // cannot alias. Stealing is what makes full bus re-creation possible.
        let (i2c, sda, scl) = unsafe { (I2C0::new(), Gpio21::new(), Gpio7::new()) };
        let config = I2cConfig::new()
            .baudrate(Hertz(100_000))
            .sda_enable_pullup(true)
            .scl_enable_pullup(true)
            // The SCD30 stretches SCL for up to ~30 ms (150 ms rarely); the IDF default bus
            // timeout is far shorter and fails reads with ESP_ERR_TIMEOUT. ~105 ms is the C6 max.
            .timeout(Duration::from_millis(100).into());
        let bus = I2cDriver::new(i2c, sda, scl, &config)?;
        log::info!("I2C initialized successfully");
        Ok(Driver::new(bus, FreeRtos))
    }

    /// The driver is only missing if a bus recovery failed to re-open; retry that lazily
    /// and report "not allowed" instead of panicking.
    fn drv(&mut self) -> Result<&mut Driver<Bus, FreeRtos>, ScdError<BusError>> {
        if self.driver.is_none() {
            self.driver = Self::open().map_err(|e| log::error!("I2C re-open failed: {e}")).ok();
        }
        self.driver.as_mut().ok_or(ScdError::NotAllowed)
    }
}

impl Scd30 for HwScd30 {
    type Error = ScdError<BusError>;

    fn firmware_version(&mut self) -> Result<(u8, u8), Self::Error> {
        self.drv()?.read_firmware_version()
    }

    fn read_setting(&mut self, setting: Setting) -> Result<u16, Self::Error> {
        let d = self.drv()?;
        match setting {
            Setting::MeasurementInterval => d.get_measurement_interval(),
            Setting::AutoSelfCalibration => d.get_automatic_self_calibration().map(u16::from),
            Setting::TemperatureOffset => d.get_temperature_offset(),
        }
    }

    fn write_setting(&mut self, setting: Setting, value: u16) -> Result<(), Self::Error> {
        let d = self.drv()?;
        match setting {
            Setting::MeasurementInterval => d.set_measurement_interval(value),
            Setting::AutoSelfCalibration => d.enable_automatic_self_calibration(value != 0),
            Setting::TemperatureOffset => d.set_temperature_offset(value),
        }
    }

    fn start_continuous_measurement(&mut self) -> Result<(), Self::Error> {
        // 0 = no ambient pressure compensation (default 1013.25 mbar), as in the C++ firmware
        self.drv()?.start_continuous_measurement(0)
    }

    fn data_ready(&mut self) -> Result<bool, Self::Error> {
        self.drv()?.data_ready()
    }

    fn read_measurement(&mut self) -> Result<Reading, Self::Error> {
        // libscd truncates CO2 to whole ppm (`as u16`); the converter rounds to ppm anyway.
        self.drv()?.read_measurement().map(|m| Reading {
            co2_ppm: f32::from(m.co2),
            temperature_c: m.temperature,
            humidity_pct: m.humidity,
        })
    }

    fn force_recalibration(&mut self, reference_ppm: u16) -> Result<(), Self::Error> {
        self.drv()?.set_forced_recalibration_value(reference_ppm)
    }

    fn recover_bus(&mut self) {
        log::warn!("Attempting full I2C bus/device reset...");
        self.driver = None; // drop releases I2C0 + pins before re-opening
        FreeRtos::delay_ms(100);
        self.driver = Self::open().map_err(|e| log::error!("I2C re-open failed: {e}")).ok();
    }
}

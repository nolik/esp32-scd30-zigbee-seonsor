mod clock;
mod sensor_hw;
mod store;
mod zb;

use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use esp_idf_svc::nvs::EspDefaultNvsPartition;
use logic::calib::MEASUREMENT_INTERVAL_S;
use logic::sensor::{Config, Reporter, SensorService};

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    // NVS before the Zigbee platform config (avoids "phy_init: NVS has not been initialized").
    let nvs = EspDefaultNvsPartition::take()?;
    let (recal_tx, recal_rx) = mpsc::channel::<()>();
    zb::start(recal_tx)?;
    let sensor_thread = run_sensor(nvs, zb::ZigbeeReporter, recal_rx)?;
    sensor_thread.join().map_err(|_| anyhow::anyhow!("sensor thread panicked"))?;
    Ok(())
}

/// The sensor thread is the sole owner of the I2C bus (Q16): the driver is created and
/// used only here, and FRC requests arrive as messages - never from another task.
fn run_sensor(
    nvs: EspDefaultNvsPartition,
    reporter: impl Reporter + Send + 'static,
    recalibration_requests: Receiver<()>,
) -> anyhow::Result<std::thread::JoinHandle<()>> {
    let store = store::NvsStoredReference::new(nvs)?;
    Ok(std::thread::Builder::new().name("scd30".into()).stack_size(8 * 1024).spawn(move || {
        let sensor = match sensor_hw::HwScd30::new() {
            Ok(sensor) => sensor,
            Err(e) => {
                log::error!("SCD30 I2C init failed: {e}");
                return;
            }
        };
        let config = Config { self_heal_enabled: cfg!(feature = "self-heal") };
        let mut service = SensorService::new(sensor, store, reporter, clock::SystemClock::new(), config);
        service.boot();
        loop {
            service.tick();
            match recalibration_requests.recv_timeout(Duration::from_secs(u64::from(MEASUREMENT_INTERVAL_S))) {
                Ok(()) => {
                    service.request_reference_recalibration();
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    std::thread::sleep(Duration::from_secs(u64::from(MEASUREMENT_INTERVAL_S)));
                }
            }
        }
    })?)
}

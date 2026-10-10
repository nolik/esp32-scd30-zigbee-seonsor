mod clock;
mod sensor_hw;
mod store;

use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use esp_idf_svc::nvs::EspDefaultNvsPartition;
use logic::calib::MEASUREMENT_INTERVAL_S;
use logic::sensor::{Config, Reporter, SensorService};
use logic::zcl::Report;

/// M1 stand-in until Zigbee reporting exists (Task 10).
struct LogReporter;

impl Reporter for LogReporter {
    fn report(&mut self, _report: &Report) {}
    fn reset_recalibration_switch(&mut self) {
        log::info!("(no Zigbee yet) FRC switch reset");
    }
}

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let nvs = EspDefaultNvsPartition::take()?;
    let (recal_tx, recal_rx) = mpsc::channel::<()>();
    let sensor_thread = run_sensor(nvs, LogReporter, recal_rx)?;
    let _keep_frc_sender_alive = recal_tx; // Task 11 hands it to the Zigbee module
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

/// ZBOSS requires the application to define this symbol; Task 9 replaces it with the real one.
#[no_mangle]
pub extern "C" fn esp_zb_app_signal_handler(_signal: *mut esp_idf_svc::sys::zb::esp_zb_app_signal_t) {}

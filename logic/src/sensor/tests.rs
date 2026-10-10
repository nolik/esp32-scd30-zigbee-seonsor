use super::fakes::{Call, FakeClock, FakeReporter, FakeSensor, FakeStore};
use super::*;

pub(super) type Svc = SensorService<FakeSensor, FakeStore, FakeReporter, FakeClock>;

pub(super) fn svc(sensor: FakeSensor, stored: Option<i32>) -> Svc {
    SensorService::new(sensor, FakeStore::with(stored), FakeReporter::default(),
                       FakeClock::default(), Config { self_heal_enabled: true })
}

#[test]
fn boot_writes_only_settings_that_differ() {
    let mut sensor = FakeSensor::new();
    sensor.settings.insert(Setting::AutoSelfCalibration, 1);
    sensor.settings.insert(Setting::TemperatureOffset, 460);
    let mut s = svc(sensor, None);
    s.boot();
    let (sensor, ..) = s.parts();
    assert_eq!(sensor.writes(), vec![(Setting::AutoSelfCalibration, 0), (Setting::TemperatureOffset, 320)]);
    assert_eq!(sensor.count(Call::Start), 1);
}

#[test]
fn boot_restores_normal_interval_left_at_frc_interval() {
    // Power cut during a Reference Recalibration leaves the sensor at 2 s (it persists in sensor NVM).
    let mut sensor = FakeSensor::new();
    sensor.settings.insert(Setting::MeasurementInterval, 2);
    let mut s = svc(sensor, None);
    s.boot();
    assert_eq!(s.parts().0.writes(), vec![(Setting::MeasurementInterval, 15)]);
}

#[test]
fn boot_writes_settings_when_they_cannot_be_read() {
    let mut sensor = FakeSensor::new();
    sensor.read_setting_fails = true;
    let mut s = svc(sensor, None);
    s.boot();
    assert_eq!(s.parts().0.writes(), vec![
        (Setting::MeasurementInterval, 15),
        (Setting::AutoSelfCalibration, 0),
        (Setting::TemperatureOffset, 320),
    ]);
}

#[test]
fn setup_commands_retry_with_backoff_and_periodic_bus_recovery() {
    let mut sensor = FakeSensor::new();
    sensor.fail_start = 11;
    let mut s = svc(sensor, None);
    s.boot();
    let (sensor, _, _, clock) = s.parts();
    assert_eq!(sensor.count(Call::Start), 12);
    assert_eq!(sensor.count(Call::RecoverBus), 2); // after failures 5 and 10
    let backoff: Vec<u64> = clock.sleeps.iter().copied().filter(|ms| *ms >= 200).collect();
    assert_eq!(backoff, [vec![200; 9], vec![2000; 2]].concat());
}

#[test]
fn boot_schedules_self_heal_only_for_plausible_reference() {
    for (stored, expect_pending) in [(Some(436), true), (None, false), (Some(150), false), (Some(20_000), false)] {
        let mut s = svc(FakeSensor::new(), stored);
        s.boot();
        assert_eq!(s.self_heal_pending(), expect_pending, "stored={stored:?}");
    }
}

#[test]
fn boot_skips_self_heal_when_disabled_by_config() {
    let mut s = SensorService::new(FakeSensor::new(), FakeStore::with(Some(436)), FakeReporter::default(),
                                   FakeClock::default(), Config { self_heal_enabled: false });
    s.boot();
    assert!(!s.self_heal_pending());
}

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
fn booted(stored: Option<i32>) -> Svc {
    let mut s = svc(FakeSensor::new(), stored);
    s.boot();
    s.parts_mut().0.calls.clear();
    s
}

fn advance(s: &mut Svc, ms: u64) {
    s.parts_mut().3.now += ms;
}

#[test]
fn no_sensor_traffic_or_reports_while_self_heal_is_pending() {
    let mut s = booted(Some(436));
    advance(&mut s, 60_000);
    s.tick();
    let (sensor, _, reporter, _) = s.parts();
    assert!(sensor.calls.is_empty());
    assert!(reporter.reports.is_empty());
}

#[test]
fn self_heal_fires_frc_with_clamped_reference_and_discards_post_frc_transient() {
    let mut s = booted(Some(2500));
    advance(&mut s, SELF_HEAL_WAIT_MS);
    s.parts_mut().0.readings.push_back(Ok(Reading { co2_ppm: 1.0, temperature_c: 21.0, humidity_pct: 40.0 }));
    s.tick();
    assert_eq!(s.parts().0.frcs(), vec![2000]);
    assert!(s.parts().2.reports.is_empty()); // the ~1 ppm transient is not published
    assert!(!s.self_heal_pending());
    s.tick();
    assert_eq!(s.parts().2.reports.len(), 1);
}

#[test]
fn trusted_session_saves_every_20th_plausible_reading_rounded() {
    let mut s = booted(Some(436));
    advance(&mut s, SELF_HEAL_WAIT_MS);
    s.parts_mut().0.default_reading.co2_ppm = 612.6;
    for _ in 0..41 {
        s.tick(); // the first reading after the FRC is the discarded transient
    }
    assert_eq!(s.parts().1.saves, vec![613, 613]);
}

#[test]
fn implausible_and_nan_readings_are_reported_but_never_counted_or_saved() {
    let mut s = booted(Some(436));
    advance(&mut s, SELF_HEAL_WAIT_MS);
    let sensor = s.parts_mut().0;
    for i in 0..30 {
        let co2 = if i % 2 == 0 { 1.0 } else { f32::NAN };
        sensor.readings.push_back(Ok(Reading { co2_ppm: co2, temperature_c: 21.0, humidity_pct: 40.0 }));
    }
    for _ in 0..30 {
        s.tick();
    }
    assert!(s.parts().1.saves.is_empty());
    assert_eq!(s.parts().2.reports.len(), 29); // first one is the discarded Post-FRC Transient
}

#[test]
fn failed_self_heal_leaves_session_untrusted() {
    let mut sensor = FakeSensor::new();
    sensor.frc_error = Some("nack");
    let mut s = svc(sensor, Some(436));
    s.boot();
    advance(&mut s, SELF_HEAL_WAIT_MS);
    for _ in 0..40 {
        s.tick();
    }
    assert!(s.parts().1.saves.is_empty());
}

#[test]
fn no_reference_means_immediate_reporting_but_no_saving() {
    let mut s = booted(None);
    for _ in 0..25 {
        s.tick();
    }
    assert_eq!(s.parts().2.reports.len(), 25);
    assert!(s.parts().1.saves.is_empty());
}

#[test]
fn data_not_ready_is_neither_a_report_nor_a_failure() {
    let mut s = booted(None);
    for _ in 0..6 {
        s.parts_mut().0.ready.push_back(Ok(false));
    }
    for _ in 0..6 {
        s.tick();
    }
    let (sensor, _, reporter, _) = s.parts();
    assert!(reporter.reports.is_empty());
    assert_eq!(sensor.count(Call::RecoverBus), 0);
}

#[test]
fn five_consecutive_failures_trigger_one_bus_recovery() {
    let mut s = booted(None);
    let sensor = s.parts_mut().0;
    sensor.ready.extend([Err("i2c"), Err("i2c"), Ok(true), Err("i2c"), Err("i2c"), Err("i2c")]);
    sensor.readings.extend([Err("crc"), Err("crc")]);
    for _ in 0..6 {
        s.tick();
    }
    // failures: e, e, read-crc (3), e (4), e (5) -> recovery; 6th tick: read-crc (1)
    assert_eq!(s.parts().0.count(Call::RecoverBus), 1);
}
#[test]
fn reference_recalibration_runs_sensirion_procedure_and_restores_normal_interval() {
    let mut s = booted(None);
    assert!(s.request_reference_recalibration());
    assert_eq!(s.parts().0.writes(), vec![(Setting::MeasurementInterval, 2)]);

    advance(&mut s, 60_000);
    s.tick(); // still settling: keeps reporting, no FRC yet
    assert!(s.parts().0.frcs().is_empty());
    assert_eq!(s.parts().2.reports.len(), 1);

    advance(&mut s, 60_000);
    s.tick();
    let (sensor, store, reporter, _) = s.parts();
    assert_eq!(sensor.frcs(), vec![425]);
    assert_eq!(sensor.writes(), vec![(Setting::MeasurementInterval, 2), (Setting::MeasurementInterval, 15)]);
    assert_eq!(store.saves, vec![425]);
    assert_eq!(reporter.switch_resets, 1);
    assert_eq!(reporter.reports.len(), 2);

    s.tick(); // first reading after the FRC: Post-FRC Transient, discarded
    assert_eq!(s.parts().2.reports.len(), 2);
}

#[test]
fn successful_reference_recalibration_starts_trusted_session() {
    // Q4: a fresh board (no Stored Reference) must start saving after a Reference Recalibration.
    let mut s = booted(None);
    s.request_reference_recalibration();
    advance(&mut s, SELF_HEAL_WAIT_MS);
    s.tick(); // finishes the recalibration, saves 425
    for _ in 0..21 {
        s.tick(); // 1 discarded transient + 20 counted readings
    }
    assert_eq!(s.parts().1.saves, vec![425, 500]);
}

#[test]
fn second_request_while_settling_is_ignored() {
    let mut s = booted(None);
    assert!(s.request_reference_recalibration());
    assert!(!s.request_reference_recalibration());
    assert_eq!(s.parts().0.writes().len(), 1);
}

#[test]
fn reference_recalibration_supersedes_pending_self_heal() {
    let mut s = booted(Some(436));
    advance(&mut s, 30_000);
    assert!(s.request_reference_recalibration());
    advance(&mut s, SELF_HEAL_WAIT_MS);
    s.tick();
    assert_eq!(s.parts().0.frcs(), vec![425]); // never 436
    assert!(!s.self_heal_pending());
}

#[test]
fn failed_reference_recalibration_still_restores_interval_and_resets_switch_without_saving() {
    let mut s = booted(None);
    s.parts_mut().0.frc_error = Some("nack");
    s.request_reference_recalibration();
    advance(&mut s, SELF_HEAL_WAIT_MS);
    s.tick();
    let (sensor, store, reporter, _) = s.parts();
    assert_eq!(sensor.writes().last(), Some(&(Setting::MeasurementInterval, 15)));
    assert!(store.saves.is_empty());
    assert_eq!(reporter.switch_resets, 1);
}

#[test]
fn interval_switch_failures_are_retried() {
    let mut s = booted(None);
    s.parts_mut().0.fail_writes = 2;
    s.request_reference_recalibration();
    assert_eq!(s.parts().0.writes(), vec![(Setting::MeasurementInterval, 2); 3]);
}
